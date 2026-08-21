use thiserror::Error;

#[derive(Debug, Error)]
pub enum MasysError {
    #[error("system query failed: {0}")]
    System(String),
    #[error("systemctl command failed: {0}")]
    Command(String),
    #[error("platform query failed: {0}")]
    Platform(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
