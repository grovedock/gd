/// Domain errors. Adapters map their failures into [`Error::Storage`]; transports map
/// each variant onto their own status codes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("corrupt object: {0}")]
    Corrupt(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// A pre-land check refused the change.
    #[error("rejected: {0}")]
    Rejected(String),
    #[error("feature `{0}` is disabled")]
    Disabled(&'static str),
    #[error("storage: {0}")]
    Storage(#[from] anyhow::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
}
