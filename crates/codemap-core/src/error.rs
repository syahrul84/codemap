use std::path::PathBuf;

/// Errors shared across codemap crates.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("invalid {path}")]
    TomlParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("SQLite error")]
    Sqlite(#[from] rusqlite::Error),

    #[error("JSON error")]
    Json(#[from] serde_json::Error),

    #[error("file walk error")]
    Walk(#[from] ignore::Error),

    #[error(
        "index at {path} has schema version {found}, newer than this codemap supports \
         ({supported}); delete the index file to rebuild it"
    )]
    IndexTooNew {
        path: PathBuf,
        found: i64,
        supported: i64,
    },

    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn io_error_keeps_path_and_source() {
        let err = Error::io(
            "app/Models/Order.php",
            std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        );
        assert_eq!(err.to_string(), "I/O error at app/Models/Order.php");
        assert_eq!(err.source().unwrap().to_string(), "missing");
    }

    #[test]
    fn config_and_not_implemented_display() {
        assert_eq!(
            Error::Config("bad level".into()).to_string(),
            "invalid configuration: bad level"
        );
        assert_eq!(
            Error::NotImplemented("scan").to_string(),
            "not implemented yet: scan"
        );
    }
}
