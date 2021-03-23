use std::{borrow::Cow, fmt::Display};

use serde::{Deserialize, Serialize};

use crate::schema::Schema;

/// a unique collection id. Choose collection names that aren't likely to
/// conflict with others, so that if someone mixes collections from multiple
/// authors in a single database.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct Id(pub Cow<'static, str>);

impl From<&'static str> for Id {
    fn from(str: &'static str) -> Self {
        Self(Cow::from(str))
    }
}

impl From<String> for Id {
    fn from(str: String) -> Self {
        Self(Cow::from(str))
    }
}

impl Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// a namespaced collection of `Document<Self>` items and views
pub trait Collection: Send + Sync {
    /// the `Id` of this collection
    fn id() -> Id;

    /// implementors define all of their `View`s in `schema`
    fn define_views(schema: &mut Schema);
}
