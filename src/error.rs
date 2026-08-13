//! Error types for outlet-postgres.

use thiserror::Error;

/// Errors that can occur when using the PostgreSQL handler.
#[derive(Error, Debug)]
pub enum PostgresHandlerError {
    /// Database connection error.
    #[error("Failed to connect to database: {0}")]
    Connection(#[from] sqlx::Error),

    /// Database migration error.
    #[error("Database migration failed: {0}")]
    Migration(sqlx::migrate::MigrateError),

    /// Database query error.
    #[error("Database query failed: {0}")]
    Query(sqlx::Error),

    /// JSON serialization error.
    #[error("JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),

    /// Caller supplied an unsafe or unsupported maintenance value.
    #[error("Invalid maintenance argument: {0}")]
    InvalidMaintenanceArgument(String),

    /// A partition operation could not prove that the requested action was safe.
    #[error("Unsafe partition maintenance operation: {0}")]
    UnsafePartitionOperation(String),
}
