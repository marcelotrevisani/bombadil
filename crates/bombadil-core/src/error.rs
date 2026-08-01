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
}
