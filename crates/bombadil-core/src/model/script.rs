use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_true() -> bool {
    true
}

/// A script run before a virtual environment is activated.
///
/// Variables it exports are captured and folded into the composed environment;
/// that behaviour is implemented in plan 2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Script {
    pub path: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}
