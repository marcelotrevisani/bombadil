use serde::{Deserialize, Serialize};

/// Whether an index replaces PyPI or supplements it.
///
/// Maps to `UV_DEFAULT_INDEX` and `UV_INDEX` respectively when the environment
/// is composed in plan 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexKind {
    Default,
    Extra,
}

/// How to authenticate to an index. Never holds secret material.
///
/// `Token` is expressed to uv as a username of `__token__` with the token as
/// the password, the convention PyPI-compatible registries use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IndexAuth {
    None,
    Basic { username: String },
    Token,
}

/// A named package index, defined once globally and opted into per project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// Must match `[A-Za-z0-9_-]+`; becomes part of `UV_INDEX_<NAME>_USERNAME`.
    pub name: String,
    pub url: String,
    pub kind: IndexKind,
    #[serde(default)]
    pub default_for_new_projects: bool,
    pub auth: IndexAuth,
}
