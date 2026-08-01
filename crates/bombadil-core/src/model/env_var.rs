use serde::{Deserialize, Serialize};

/// A single environment variable entry.
///
/// When `secret` is true, `value` is always empty in the persisted config: the
/// real value lives in the OS keychain under `env/<project-id>/<key>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
}
