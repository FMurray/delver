use thiserror::Error;

/// Errors surfaced by the persistent index.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    /// PDF could not be loaded or parsed by delver-core.
    #[error("pdf parse error: {0}")]
    Pdf(String),

    /// Stored rows violate the schema contract (should never happen).
    #[error("stored data invalid: {0}")]
    Corrupt(String),

    /// Failed to build the blocking tokio runtime.
    #[error("tokio runtime error: {0}")]
    Runtime(#[from] std::io::Error),
}
