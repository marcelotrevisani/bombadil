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
