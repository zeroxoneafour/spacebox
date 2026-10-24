use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("{}: {message}", path.display())]
    Invalid { path: PathBuf, message: String },
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub(crate) fn invalid(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Error::Invalid {
            path: path.into(),
            message: message.into(),
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
