use std::time::Duration;

use bonsaidb::{
    client::{url::Url, Client, RemoteDatabase},
    core::{
        actionable::Permissions,
        admin::{Admin, PermissionGroup, ADMIN_DATABASE_NAME},
        circulate::flume,
        keyvalue::AsyncKeyValue,
        permissions::{
            bonsai::{BonsaiAction, ServerAction},
            Statement,
        },
        schema::{InsertError, SerializedCollection},
        test_util::{BasicSchema, HarnessTest, TestDirectory},
    },
    local::config::Builder,
    server::{
        fabruic::Certificate,
        test_util::{initialize_basic_server, BASIC_SERVER_NAME},
        DefaultPermissions, Server, ServerConfiguration,
    },
};
use bonsaidb_core::connection::{Authentication, AuthenticationMethod, SensitiveString};
use once_cell::sync::Lazy;
use rand::{distributions::Alphanumeric, Rng};
use tokio::sync::Mutex;

const INCOMPATIBLE_PROTOCOL_VERSION: &str = "otherprotocol";

async fn initialize_shared_server() -> Certificate {
    static CERTIFICATE: Lazy<Mutex<Option<Certificate>>> = Lazy::new(|| Mutex::new(None));
    drop(env_logger::try_init());
    let mut certificate = CERTIFICATE.lock().await;
    if certificate.is_none() {
        let (sender, receiver) = flume::bounded(1);
        std::thread::spawn(|| run_shared_server(sender));

        *certificate = Some(receiver.recv_async().await.unwrap());
        // Give the server time to start listening
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }

    certificate.clone().unwrap()
}

fn run_shared_server(certificate_sender: flume::Sender<Certificate>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let directory = TestDirectory::new("shared-server");
        let server = initialize_basic_server(directory.as_ref()).await.unwrap();
        certificate_sender
            .send(
                server
                    .certificate_chain()
                    .await
                    .unwrap()
                    .into_end_entity_certificate(),
            )
            .unwrap();

        #[cfg(feature = "websockets")]
        {
            let task_server = server.clone();
            tokio::spawn(async move {
                task_server
                    .listen_for_websockets_on("localhost:6001", false)
                    .await
                    .unwrap();
            });
        }

        server.listen_on(6000).await.unwrap();
    });

    Ok(())
}

#[cfg(feature = "websockets")]
mod websockets {
    use tokio::runtime::Runtime;

    use super::*;

    struct WebsocketTestHarness {
        client: Client,
        url: Url,
        db: RemoteDatabase,
    }

    impl WebsocketTestHarness {
        pub async fn new(test: HarnessTest) -> anyhow::Result<Self> {
            use bonsaidb_core::connection::AsyncStorageConnection;

            initialize_shared_server().await;
            let url = Url::parse("ws://localhost:6001")?;
            let client = Client::new(url.clone())?;

            let dbname = format!("websockets-{}", test);
            client
                .create_database::<BasicSchema>(&dbname, false)
                .await?;
            let db = client.database::<BasicSchema>(&dbname).await?;

            Ok(Self { client, url, db })
        }

        pub const fn server_name() -> &'static str {
            "websocket"
        }

        pub fn server(&self) -> &Client {
            &self.client
        }

        pub async fn connect(&self) -> anyhow::Result<RemoteDatabase> {
            Ok(self.db.clone())
        }

        #[allow(dead_code)] // We will want this in the future but it's currently unused
        pub async fn connect_with_permissions(
            &self,
            permissions: Vec<Statement>,
            label: &str,
        ) -> anyhow::Result<RemoteDatabase> {
            let client = Client::new(self.url.clone())?;
            assume_permissions(client, label, self.db.name(), permissions).await
        }

        pub async fn shutdown(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    bonsaidb_core::define_async_connection_test_suite!(WebsocketTestHarness);

    bonsaidb_core::define_async_pubsub_test_suite!(WebsocketTestHarness);
    bonsaidb_core::define_async_kv_test_suite!(WebsocketTestHarness);

    struct BlockingWebsocketTestHarness {
        client: Client,
        // url: Url,
        db: RemoteDatabase,
    }

    impl BlockingWebsocketTestHarness {
        pub fn new(test: HarnessTest) -> anyhow::Result<Self> {
            use bonsaidb_core::connection::StorageConnection;
            let runtime = Runtime::new()?;
            runtime.block_on(initialize_shared_server());
            let url = Url::parse("ws://localhost:6001")?;
            let client = Client::new(url)?;

            let dbname = format!("blocking-websockets-{}", test);
            client.create_database::<BasicSchema>(&dbname, false)?;
            let db = client.database::<BasicSchema>(&dbname)?;

            Ok(Self { client, db })
        }

        pub const fn server_name() -> &'static str {
            "websocket-blocking"
        }

        pub fn server(&self) -> &Client {
            &self.client
        }

        pub fn connect(&self) -> anyhow::Result<RemoteDatabase> {
            Ok(self.db.clone())
        }

        // #[allow(dead_code)] // We will want this in the future but it's currently unused
        // pub  fn connect_with_permissions(
        //     &self,
        //     permissions: Vec<Statement>,
        //     label: &str,
        // ) -> anyhow::Result<RemoteDatabase> {
        //     let client = Client::new(self.url.clone())?;
        //     assume_permissions(client, label, self.db.name(), permissions)
        // }

        pub fn shutdown(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn incompatible_client_version() -> anyhow::Result<()> {
        let certificate = initialize_shared_server().await;

        let url = Url::parse("ws://localhost:6001")?;
        let client = Client::build(url.clone())
            .with_certificate(certificate.clone())
            .with_protocol_version(INCOMPATIBLE_PROTOCOL_VERSION)
            .finish()?;

        check_incompatible_client(client).await
    }

    bonsaidb_core::define_blocking_connection_test_suite!(BlockingWebsocketTestHarness);

    bonsaidb_core::define_blocking_pubsub_test_suite!(BlockingWebsocketTestHarness);
    bonsaidb_core::define_blocking_kv_test_suite!(BlockingWebsocketTestHarness);
}

mod bonsai {
    use super::*;
    struct BonsaiTestHarness {
        client: Client,
        url: Url,
        certificate: Certificate,
        db: RemoteDatabase,
    }

    impl BonsaiTestHarness {
        pub async fn new(test: HarnessTest) -> anyhow::Result<Self> {
            use bonsaidb_core::connection::AsyncStorageConnection;
            let certificate = initialize_shared_server().await;

            let url = Url::parse(&format!(
                "bonsaidb://localhost:6000?server={}",
                BASIC_SERVER_NAME
            ))?;
            let client = Client::build(url.clone())
                .with_certificate(certificate.clone())
                .finish()?;

            let dbname = format!("bonsai-{}", test);
            client
                .create_database::<BasicSchema>(&dbname, false)
                .await?;
            let db = client.database::<BasicSchema>(&dbname).await?;

            Ok(Self {
                client,
                url,
                certificate,
                db,
            })
        }

        pub fn server_name() -> &'static str {
            "bonsai"
        }

        pub fn server(&self) -> &'_ Client {
            &self.client
        }

        pub async fn connect<'a, 'b>(&'a self) -> anyhow::Result<RemoteDatabase> {
            Ok(self.db.clone())
        }

        #[allow(dead_code)] // We will want this in the future but it's currently unused
        pub async fn connect_with_permissions(
            &self,
            statements: Vec<Statement>,
            label: &str,
        ) -> anyhow::Result<RemoteDatabase> {
            let client = Client::build(self.url.clone())
                .with_certificate(self.certificate.clone())
                .finish()?;
            assume_permissions(client, label, self.db.name(), statements).await
        }

        pub async fn shutdown(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn incompatible_client_version() -> anyhow::Result<()> {
        let certificate = initialize_shared_server().await;

        let url = Url::parse(&format!(
            "bonsaidb://localhost:6000?server={}",
            BASIC_SERVER_NAME
        ))?;
        let client = Client::build(url.clone())
            .with_certificate(certificate.clone())
            .with_protocol_version(INCOMPATIBLE_PROTOCOL_VERSION)
            .finish()?;

        check_incompatible_client(client).await
    }

    bonsaidb_core::define_async_connection_test_suite!(BonsaiTestHarness);
    bonsaidb_core::define_async_pubsub_test_suite!(BonsaiTestHarness);
    bonsaidb_core::define_async_kv_test_suite!(BonsaiTestHarness);
}

async fn check_incompatible_client(client: Client) -> anyhow::Result<()> {
    use bonsaidb_core::connection::AsyncStorageConnection;
    match client
        .database::<()>("a database")
        .await?
        .set_numeric_key("a", 1_u64)
        .await
    {
        Err(bonsaidb_core::Error::Client(err)) => {
            assert!(
                err.contains("protocol version"),
                "unexpected error: {:?}",
                err
            );
        }
        other => unreachable!(
            "Unexpected result with invalid protocol version: {:?}",
            other
        ),
    }

    Ok(())
}

#[allow(dead_code)] // We will want this in the future but it's currently unused
async fn assume_permissions(
    connection: Client,
    label: &str,
    database_name: &str,
    statements: Vec<Statement>,
) -> anyhow::Result<RemoteDatabase> {
    use bonsaidb_core::connection::AsyncStorageConnection;
    let username = format!("{}-{}", database_name, label);
    let password = SensitiveString(
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(8)
            .map(char::from)
            .collect(),
    );
    match connection.create_user(&username).await {
        Ok(user_id) => {
            connection
                .set_user_password(&username, password.clone())
                .await
                .unwrap();

            // Create an administrators permission group, or get its ID if it already existed.
            let admin = connection.database::<Admin>(ADMIN_DATABASE_NAME).await?;
            let administrator_group_id = match (PermissionGroup {
                name: String::from(label),
                statements,
            }
            .push_into_async(&admin)
            .await)
            {
                Ok(doc) => doc.header.id,
                Err(InsertError {
                    error:
                        bonsaidb_core::Error::UniqueKeyViolation {
                            existing_document, ..
                        },
                    ..
                }) => existing_document.id.deserialize()?,
                Err(other) => anyhow::bail!(other),
            };

            // Make our user a member of the administrators group.
            connection
                .add_permission_group_to_user(user_id, administrator_group_id)
                .await
                .unwrap();
        }
        Err(bonsaidb_core::Error::UniqueKeyViolation { .. }) => {}
        Err(other) => anyhow::bail!(other),
    };

    connection
        .authenticate(Authentication::password(username, password)?)
        .await
        .unwrap();

    Ok(connection.database::<BasicSchema>(database_name).await?)
}

#[tokio::test]
async fn authenticated_permissions_test() -> anyhow::Result<()> {
    use bonsaidb_core::connection::AsyncStorageConnection;
    let database_path = TestDirectory::new("authenticated-permissions");
    let server = Server::open(
        ServerConfiguration::new(&database_path)
            .default_permissions(Permissions::from(
                Statement::for_any()
                    .allowing(&BonsaiAction::Server(ServerAction::Connect))
                    .allowing(&BonsaiAction::Server(ServerAction::Authenticate(
                        AuthenticationMethod::PasswordHash,
                    ))),
            ))
            .authenticated_permissions(DefaultPermissions::AllowAll),
    )
    .await?;
    server.install_self_signed_certificate(false).await?;
    let certificate = server
        .certificate_chain()
        .await?
        .into_end_entity_certificate();

    server.create_user("ecton").await?;
    server
        .set_user_password("ecton", SensitiveString("hunter2".to_string()))
        .await?;
    tokio::spawn(async move {
        server.listen_on(6002).await?;
        Result::<(), anyhow::Error>::Ok(())
    });
    // Give the server time to listen
    tokio::time::sleep(Duration::from_millis(10)).await;

    let url = Url::parse("bonsaidb://localhost:6002")?;
    let client = Client::build(url).with_certificate(certificate).finish()?;
    match client.create_user("otheruser").await {
        Err(bonsaidb_core::Error::PermissionDenied(_)) => {}
        other => unreachable!(
            "should not have permission to create another user before logging in: {other:?}"
        ),
    }

    let authenticated_client = client
        .authenticate(Authentication::password(
            "ecton",
            SensitiveString(String::from("hunter2")),
        )?)
        .await
        .unwrap();
    authenticated_client
        .create_user("otheruser")
        .await
        .expect("should be able to create user after logging in");

    Ok(())
}
