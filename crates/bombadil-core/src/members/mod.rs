mod discover;
mod relative;
mod workspace;

pub use discover::{DiscoveredMember, MAX_DISCOVERY_DEPTH, SKIPPED_DIRS, discover};
pub use relative::path_for_manifest;
pub use workspace::{WorkspaceKind, glob_matches, parse_workspace};
