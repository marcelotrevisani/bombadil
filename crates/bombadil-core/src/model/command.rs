use serde::{Deserialize, Serialize};

/// A command a person saved on a project's Run tab, run as
/// `uv run <command>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedCommand {
    pub label: String,
    pub command: String,
}
