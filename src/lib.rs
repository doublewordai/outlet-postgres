//! # outlet-postgres
//!
//! PostgreSQL logging handler for the outlet HTTP request/response middleware.
//! This crate implements the `RequestHandler` trait from outlet to log HTTP
//! requests and responses to PostgreSQL with JSONB serialization for bodies.
//!
//! ## Features
//!
//! - **PostgreSQL Integration**: Uses sqlx for async PostgreSQL operations
//! - **JSONB Bodies**: Serializes request/response bodies to JSONB fields
//! - **Type-safe Querying**: Query logged data with typed request/response bodies
//! - **Correlation**: Links requests and responses via correlation IDs
//! - **Error Handling**: Graceful error handling with logging
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use outlet::{RequestLoggerLayer, RequestLoggerConfig};
//! use outlet_postgres::{PostgresHandler, repository::RequestFilter};
//! use axum::{routing::get, Router};
//! use tower::ServiceBuilder;
//! use serde::{Deserialize, Serialize};
//!
//! // Define your custom request and response body types
//! #[derive(Deserialize, Serialize)]
//! struct ApiRequest {
//!     user_id: u64,
//!     action: String,
//! }
//!
//! #[derive(Deserialize, Serialize)]
//! struct ApiResponse {
//!     success: bool,
//!     message: String,
//! }
//!
//! async fn hello() -> &'static str {
//!     "Hello, World!"
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let database_url = "postgresql://user:password@localhost/dbname";
//!     
//!     // Create handler with separate request and response types
//!     let handler = PostgresHandler::<ApiRequest, ApiResponse>::new(database_url).await?;
//!     
//!     // Or use default Value types for flexible JSON storage
//!     // let handler = PostgresHandler::new(database_url).await?;
//!     
//!     let layer = RequestLoggerLayer::new(RequestLoggerConfig::default(), handler.clone());
//!
//!     // Get a repository for querying logged requests and responses
//!     let repository = handler.repository();
//!     
//!     // Query logged data with filters
//!     let filter = RequestFilter {
//!         method: Some("POST".to_string()),
//!         status_code_min: Some(400),
//!         limit: Some(10),
//!         ..Default::default()
//!     };
//!     let results = repository.query(filter).await?;
//!     
//!     for pair in results {
//!         println!("Request: {} {}", pair.request.method, pair.request.uri);
//!         match pair.request.body {
//!             Some(Ok(request_body)) => println!("  Parsed request: {:?}", request_body),
//!             Some(Err(raw_bytes)) => println!("  Raw request bytes: {} bytes", raw_bytes.len()),
//!             None => println!("  No request body"),
//!         }
//!         
//!         if let Some(response) = pair.response {
//!             println!("Response: {} ({}ms)", response.status_code, response.duration_ms);
//!             match response.body {
//!                 Some(Ok(response_body)) => println!("  Parsed response: {:?}", response_body),
//!                 Some(Err(raw_bytes)) => println!("  Raw response bytes: {} bytes", raw_bytes.len()),
//!                 None => println!("  No response body"),
//!             }
//!         }
//!     }
//!
//!     let app = Router::new()
//!         .route("/hello", get(hello))
//!         .layer(ServiceBuilder::new().layer(layer));
//!
//!     let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
//!     axum::serve(listener, app).await?;
//!     Ok(())
//! }
//! ```

use chrono::{DateTime, Utc};
use outlet::{RequestData, RequestHandler, ResponseData};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::{debug, error, instrument};
use base64::Engine;

pub mod error;
pub mod repository;
pub use error::PostgresHandlerError;
pub use repository::{HttpRequest, HttpResponse, RequestRepository, RequestResponsePair, RequestFilter};

/// PostgreSQL handler for outlet middleware.
///
/// Implements the `RequestHandler` trait to log HTTP requests and responses
/// to PostgreSQL. Request and response bodies are serialized to JSONB fields.
///
/// Generic over `TReq` and `TRes` which represent the request and response body types
/// that should implement `Deserialize` for parsing and `Serialize` for database storage as JSONB.
/// Use `serde_json::Value` for flexible JSON storage, or custom structs for typed storage.
#[derive(Clone)]
pub struct PostgresHandler<TReq = Value, TRes = Value>
where
    TReq: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
    TRes: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
{
    pool: PgPool,
    _phantom_req: std::marker::PhantomData<TReq>,
    _phantom_res: std::marker::PhantomData<TRes>,
}

impl<TReq, TRes> PostgresHandler<TReq, TRes>
where
    TReq: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
    TRes: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
{
    /// Create a new PostgreSQL handler with a connection pool.
    ///
    /// This will attempt to connect to the database and run any necessary
    /// migrations to set up the logging tables.
    ///
    /// # Arguments
    ///
    /// * `database_url` - PostgreSQL connection string
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use outlet_postgres::PostgresHandler;
    /// use serde::{Deserialize, Serialize};
    /// use serde_json::Value;
    ///
    /// #[derive(Deserialize, Serialize)]
    /// struct MyBodyType {
    ///     id: u64,
    ///     name: String,
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///     // With custom body type
    ///     let handler = PostgresHandler::<MyBodyType>::new("postgresql://user:pass@localhost/db").await?;
    ///     
    ///     // With default JSON Value type  
    ///     let handler = PostgresHandler::<Value>::new("postgresql://user:pass@localhost/db").await?;
    ///     // or simply:
    ///     // let handler = PostgresHandler::new("postgresql://user:pass@localhost/db").await?;
    ///     Ok(())
    /// }
    /// ```
    pub async fn new(database_url: &str) -> Result<Self, PostgresHandlerError> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(PostgresHandlerError::Connection)?;

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(PostgresHandlerError::Migration)?;

        Ok(Self {
            pool,
            _phantom_req: std::marker::PhantomData,
            _phantom_res: std::marker::PhantomData,
        })
    }

    /// Create a PostgreSQL handler from an existing connection pool.
    ///
    /// Use this if you already have a connection pool and want to reuse it.
    /// This will also run migrations on the provided pool.
    ///
    /// # Arguments
    ///
    /// * `pool` - Existing PostgreSQL connection pool
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use outlet_postgres::PostgresHandler;
    /// use sqlx::PgPool;
    /// use serde::{Deserialize, Serialize};
    ///
    /// #[derive(Deserialize, Serialize)]
    /// struct MyBodyType {
    ///     id: u64,
    ///     name: String,
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<(), Box<dyn std::error::Error>> {
    ///     let pool = PgPool::connect("postgresql://user:pass@localhost/db").await?;
    ///     let handler = PostgresHandler::<MyBodyType>::from_pool(pool).await?;
    ///     Ok(())
    /// }
    /// ```
    pub async fn from_pool(pool: PgPool) -> Result<Self, PostgresHandlerError> {
        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(PostgresHandlerError::Migration)?;

        Ok(Self {
            pool,
            _phantom_req: std::marker::PhantomData,
            _phantom_res: std::marker::PhantomData,
        })
    }

    /// Convert headers to a JSONB-compatible format.
    fn headers_to_json(headers: &HashMap<String, Vec<bytes::Bytes>>) -> Value {
        let mut header_map = HashMap::new();
        for (name, values) in headers {
            if values.len() == 1 {
                let value_str = String::from_utf8_lossy(&values[0]).to_string();
                header_map.insert(name.clone(), Value::String(value_str));
            } else {
                let value_array: Vec<Value> = values
                    .iter()
                    .map(|v| Value::String(String::from_utf8_lossy(v).to_string()))
                    .collect();
                header_map.insert(name.clone(), Value::Array(value_array));
            }
        }
        serde_json::to_value(header_map).unwrap_or(Value::Null)
    }

    /// Convert bytes to a JSONB value for requests, attempting to deserialize as TReq first.
    /// If that fails, store the raw bytes as a base64-encoded string.
    fn request_body_to_json_with_fallback(body: &[u8]) -> (Value, bool) {
        // First try to deserialize as the typed request body TReq
        if let Ok(typed_value) = serde_json::from_slice::<TReq>(body) {
            if let Ok(json_value) = serde_json::to_value(typed_value) {
                return (json_value, true);
            }
        }
        
        // If deserialization fails, store as base64-encoded string
        let base64_string = base64::engine::general_purpose::STANDARD.encode(body);
        (Value::String(base64_string), false)
    }

    /// Convert bytes to a JSONB value for responses, attempting to deserialize as TRes first.
    /// If that fails, store the raw bytes as a base64-encoded string.
    fn response_body_to_json_with_fallback(body: &[u8]) -> (Value, bool) {
        // First try to deserialize as the typed response body TRes
        if let Ok(typed_value) = serde_json::from_slice::<TRes>(body) {
            if let Ok(json_value) = serde_json::to_value(typed_value) {
                return (json_value, true);
            }
        }
        
        // If deserialization fails, store as base64-encoded string
        let base64_string = base64::engine::general_purpose::STANDARD.encode(body);
        (Value::String(base64_string), false)
    }

    /// Get a repository for querying logged requests and responses.
    /// 
    /// Returns a `RequestRepository` with the same type parameters as this handler,
    /// allowing for type-safe querying of request and response bodies.
    pub fn repository(&self) -> crate::repository::RequestRepository<TReq, TRes> {
        crate::repository::RequestRepository::new(self.pool.clone())
    }
}

impl<TReq, TRes> RequestHandler for PostgresHandler<TReq, TRes>
where
    TReq: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
    TRes: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
{
    #[instrument(skip(self, data), fields(correlation_id = %correlation_id))]
    async fn handle_request(&self, data: RequestData, correlation_id: u64) {
        let headers_json = Self::headers_to_json(&data.headers);
        let (body_json, parsed) = data.body.as_ref()
            .map(|b| {
                let (json, parsed) = Self::request_body_to_json_with_fallback(b);
                (Some(json), parsed)
            })
            .unwrap_or((None, false));

        let timestamp: DateTime<Utc> = data.timestamp.into();

        let result = sqlx::query(
            r#"
            INSERT INTO http_requests (correlation_id, timestamp, method, uri, headers, body, body_parsed)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(correlation_id as i64)
        .bind(timestamp)
        .bind(data.method.to_string())
        .bind(data.uri.to_string())
        .bind(headers_json)
        .bind(body_json)
        .bind(parsed)
        .execute(&self.pool)
        .await;

        if let Err(e) = result {
            error!(correlation_id = %correlation_id, error = %e, "Failed to insert request data");
        } else {
            debug!(correlation_id = %correlation_id, "Request data inserted successfully");
        }
    }

    #[instrument(skip(self, data), fields(correlation_id = %correlation_id))]
    async fn handle_response(&self, data: ResponseData, correlation_id: u64) {
        let headers_json = Self::headers_to_json(&data.headers);
        let (body_json, parsed) = data.body.as_ref()
            .map(|b| {
                let (json, parsed) = Self::response_body_to_json_with_fallback(b);
                (Some(json), parsed)
            })
            .unwrap_or((None, false));

        let timestamp: DateTime<Utc> = data.timestamp.into();
        let duration_ms = data.duration.as_millis() as i64;

        let result = sqlx::query(
            r#"
            INSERT INTO http_responses (correlation_id, timestamp, status_code, headers, body, body_parsed, duration_ms)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(correlation_id as i64)
        .bind(timestamp)
        .bind(data.status.as_u16() as i32)
        .bind(headers_json)
        .bind(body_json)
        .bind(parsed)
        .bind(duration_ms)
        .execute(&self.pool)
        .await;

        if let Err(e) = result {
            error!(correlation_id = %correlation_id, error = %e, "Failed to insert response data");
        } else {
            debug!(correlation_id = %correlation_id, "Response data inserted successfully");
        }
    }
}
