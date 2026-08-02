mod discover;
mod workspace;

pub use discover::{DiscoveredMember, MAX_DISCOVERY_DEPTH, SKIPPED_DIRS, discover};
pub use workspace::{WorkspaceKind, glob_matches, parse_workspace};
