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

impl Index {
    /// Whether this index has not been filled in yet: no name and no URL.
    ///
    /// Pressing "Add index" has to create a row to type into, and that row
    /// starts empty. Reporting it as two configuration errors before the user
    /// has typed a character trains them to ignore the banner -- which is the
    /// one thing the banner cannot afford, since it is also how a real
    /// misconfiguration is reported.
    ///
    /// Deliberately *both* fields, not either. A half-filled index -- a name
    /// with no URL, or a URL with no name -- is a real problem that would
    /// break a sync, and still reports as one.
    pub fn is_blank(&self) -> bool {
        self.name.trim().is_empty() && self.url.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(name: &str, url: &str) -> Index {
        Index {
            name: name.into(),
            url: url.into(),
            kind: IndexKind::Extra,
            default_for_new_projects: false,
            auth: IndexAuth::None,
        }
    }

    #[test]
    fn a_freshly_added_index_is_blank() {
        assert!(index("", "").is_blank());
        assert!(index("  ", "\t").is_blank(), "whitespace is not content");
    }

    #[test]
    fn a_half_filled_index_is_not_blank() {
        // The distinction that matters. A name with no URL is not "not
        // started", it is broken, and it must still be reported.
        assert!(!index("internal", "").is_blank());
        assert!(!index("", "https://nexus.example/simple").is_blank());
    }
}
