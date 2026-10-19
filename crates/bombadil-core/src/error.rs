use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not determine a configuration directory for this platform")]
    NoHomeDirectory,

    #[error("{path} has no parent directory")]
    BadPath { path: PathBuf },

    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not serialize configuration: {source}")]
    Serialize {
        #[source]
        source: toml::ser::Error,
    },

    #[error(
        "{path} was written by a newer version of Bombadil \
         (schema version {found}, this build supports {supported}); it will not be modified"
    )]
    ReadOnly {
        path: PathBuf,
        found: u32,
        supported: u32,
    },

    #[error("{path} could not be read or parsed ({message}); it will not be modified")]
    RefusedUnreadable { path: PathBuf, message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum PyProjectError {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Produced by [`crate::pyproject::parse`], which has no path to report:
    /// it only sees text. Kept separate from [`PyProjectError::ParseAt`] so
    /// that a path-free caller (e.g. parsing an in-memory string) doesn't
    /// have to invent one.
    /// From [`crate::pyproject::set_requires_python`].
    #[error("could not edit pyproject.toml: {0}")]
    Edit(String),

    #[error("could not parse pyproject.toml: {source}")]
    Parse {
        #[source]
        source: toml::de::Error,
    },

    /// Produced by [`crate::pyproject::load`], which attaches the path it
    /// read from to whatever [`crate::pyproject::parse`] reported — the app's
    /// main screen is a list of projects, so a parse error must say which
    /// project's file failed.
    #[error("could not parse {path}: {source}")]
    ParseAt {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum VenvPathError {
    #[error("{pyproject_path} has no parent directory, so the project directory is unknown")]
    NoProjectDirectory { pyproject_path: PathBuf },
}

/// Why [`crate::venv_path::delete`] refused to remove something.
///
/// One variant per refusal rather than a single "could not delete": this is
/// the only destructive filesystem operation in the crate, and a user who is
/// told *why* it was refused can act on it, while "could not delete" invites
/// them to go and `rm -rf` it by hand.
#[derive(Debug, thiserror::Error)]
pub enum VenvDeleteError {
    #[error("the project's virtual environment path could not be resolved: {0}")]
    Resolve(#[from] VenvPathError),

    /// The caller asked to delete something that is not where this project's
    /// virtual environment lives. Nothing is touched.
    #[error(
        "{path} is not the virtual environment of this project (that is {expected}), \
         so it will not be deleted"
    )]
    OutsideProject { path: PathBuf, expected: PathBuf },

    /// No `pyvenv.cfg`, so whatever is there was not created by uv or the
    /// stdlib `venv` module. A plausible name is not evidence.
    #[error(
        "{path} holds no pyvenv.cfg, so it is not a virtual environment and will not be deleted"
    )]
    NotAVenv { path: PathBuf },

    /// A symlink or a plain file at the venv path. Deleting a symlink would
    /// either be a no-op on the environment or, if it were followed, remove
    /// whatever it points at -- neither is what "delete this venv" means.
    #[error(
        "{path} is not a real directory (it is a file or a symlink), so it will not be deleted"
    )]
    NotADirectory { path: PathBuf },

    #[error("could not delete {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum UvResolveError {
    #[error("the bundled uv could not be made available: {message}")]
    BundledUnavailable { message: String },

    #[error("settings require uv from PATH, but no runnable uv was found there")]
    PathUvUnavailable,

    #[error("settings point at {path}, which could not be run")]
    ConfiguredUnavailable { path: PathBuf },

    #[error("{path} is uv {found}, older than the minimum supported {minimum}")]
    TooOld {
        path: PathBuf,
        found: String,
        minimum: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ResultsError {
    #[error("could not parse the JSON output of `{command}`: {source}")]
    Json {
        command: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("could not run {program}: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Only `FakeCommandRunner` constructs this; gated the same way so it
    /// does not appear in production docs or need handling by every
    /// production `match` over `CommandError`.
    #[cfg(any(test, feature = "test-support"))]
    #[error("no fake outcome was programmed for {program} {args:?}")]
    Unexpected { program: PathBuf, args: Vec<String> },
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("the OS keychain is not available on this machine: {message}")]
    Unavailable { message: String },

    #[error("could not read or write the secret for {account}: {message}")]
    Backend { account: String, message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("{kind:?} is not a terminal available on {os:?}")]
    WrongOs {
        kind: crate::terminal::TerminalKind,
        os: crate::terminal::Os,
    },

    #[error("the custom terminal command is empty")]
    EmptyTemplate,
}

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("could not run the pre-activate script {script}: {message}")]
    ScriptSpawn {
        script: std::path::PathBuf,
        message: String,
    },

    #[error("the pre-activate script {script} failed (exit {status:?}): {stderr}")]
    ScriptFailed {
        script: std::path::PathBuf,
        status: Option<i32>,
        stderr: String,
    },

    #[error(
        "the pre-activate script {script} did not produce a complete environment dump; \
         it may have called `exit`"
    )]
    ScriptDumpIncomplete { script: std::path::PathBuf },

    #[error("could not read a secret while composing the environment: {message}")]
    Secret { message: String },

    #[error(
        "indexes {first:?} and {second:?} both use the credential variable {var}, \
         so one index's password would be sent to the other's server"
    )]
    IndexCredentialCollision {
        first: String,
        second: String,
        var: String,
    },

    #[error("could not build the terminal command: {0}")]
    Terminal(#[from] TerminalError),

    #[error("could not write the terminal init file to {path}: {message}")]
    TerminalInitFile { path: PathBuf, message: String },

    /// `CommandError::Spawn` carries only the program name, which is not
    /// enough for spec section 9 -- a user whose chosen terminal has been
    /// uninstalled needs to see the flags that were tried, not just that
    /// "something" failed to launch.
    #[error("could not open a terminal ({argv}): {message}")]
    TerminalSpawn { argv: String, message: String },
}
