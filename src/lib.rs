//! # outlet-postgres
//!
//! PostgreSQL logging handler for the outlet HTTP request/response middleware.
//! This crate implements the `RequestHandler` trait from outlet to log HTTP
//! requests and responses to PostgreSQL with JSONB serialization for bodies.
//!
//! ## Quick Start
//!
//! Basic usage:
//!
//! ```rust,no_run
//! use outlet::{RequestLoggerLayer, RequestLoggerConfig};
//! use outlet_postgres::PostgresHandler;
//! use axum::{routing::get, Router};
//! use tower::ServiceBuilder;
//!
//! async fn hello() -> &'static str {
//!     "Hello, World!"
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let database_url = "postgresql://user:password@localhost/dbname";
//!     let handler: PostgresHandler = PostgresHandler::new(database_url).await?;
//!     let layer = RequestLoggerLayer::new(RequestLoggerConfig::default(), handler);
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
//!
//! ## Features
//!
//! - **PostgreSQL Integration**: Uses sqlx for async PostgreSQL operations
//! - **JSONB Bodies**: Serializes request/response bodies to JSONB fields
//! - **Type-safe Querying**: Query logged data with typed request/response bodies
//! - **Correlation**: Links requests and responses via correlation IDs
//! - **Error Handling**: Graceful error handling with logging
//! - **Flexible Serialization**: Generic error handling for custom serializer types

/// Error type for serialization failures with fallback data.
///
/// When serializers fail to parse request/response bodies into structured types,
/// this error provides both the parsing error details and fallback data that
/// can be stored as a string representation.
#[derive(Debug)]
pub struct SerializationError {
    /// The fallback representation of the data (e.g., base64-encoded, raw string)
    pub fallback_data: String,
    /// The underlying error that caused serialization to fail
    pub error: Box<dyn std::error::Error + Send + Sync>,
}

impl SerializationError {
    /// Create a new serialization error with fallback data
    pub fn new(
        fallback_data: String,
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            fallback_data,
            error: Box::new(error),
        }
    }
}

impl std::fmt::Display for SerializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Serialization failed: {}", self.error)
    }
}

impl std::error::Error for SerializationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

use chrono::{DateTime, Utc};
use http::Uri;
use outlet::{RequestData, RequestHandler, ResponseData};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, instrument};
use uuid::Uuid;

pub mod error;
pub mod repository;
pub use error::PostgresHandlerError;
pub use repository::{
    HttpRequest, HttpResponse, RequestFilter, RequestRepository, RequestResponsePair,
};

/// Get the migrator for running outlet-postgres database migrations.
///
/// This returns a SQLx migrator that can be used to set up the required
/// `http_requests` and `http_responses` tables. The consuming application
/// is responsible for running these migrations at the appropriate time
/// and in the appropriate database schema.
///
/// # Examples
///
/// ```rust,no_run
/// use outlet_postgres::migrator;
/// use sqlx::PgPool;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let pool = PgPool::connect("postgresql://user:pass@localhost/db").await?;
///     
///     // Run outlet migrations
///     migrator().run(&pool).await?;
///     
///     Ok(())
/// }
/// ```
pub fn migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate!("./migrations")
}

/// Type alias for request body serializers.
///
/// Request serializers take full request context including headers and body bytes.
/// On failure, they return a `SerializationError` with fallback data.
type RequestSerializer<T> =
    Arc<dyn Fn(&outlet::RequestData) -> Result<T, SerializationError> + Send + Sync>;

/// Type alias for response body serializers.
///
/// Response serializers take both request and response context, allowing them to
/// make parsing decisions based on request details and response headers (e.g., compression).
/// On failure, they return a `SerializationError` with fallback data.
type ResponseSerializer<T> = Arc<
    dyn Fn(&outlet::RequestData, &outlet::ResponseData) -> Result<T, SerializationError>
        + Send
        + Sync,
>;

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
    request_serializer: RequestSerializer<TReq>,
    response_serializer: ResponseSerializer<TRes>,
    path_filter: Option<PathFilter>,
    instance_id: Uuid,
}

/// Filter configuration for determining which requests to log.
#[derive(Clone, Debug)]
pub struct PathFilter {
    /// Only log requests whose URI path starts with any of these prefixes.
    /// If empty, all requests are logged.
    pub allowed_prefixes: Vec<String>,
    /// Skip requests whose URI path starts with any of these prefixes.
    /// Takes precedence over allowed_prefixes.
    pub blocked_prefixes: Vec<String>,
}

impl<TReq, TRes> PostgresHandler<TReq, TRes>
where
    TReq: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
    TRes: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
{
    /// Default serializer that attempts serde JSON deserialization.
    /// On failure, returns a SerializationError with raw bytes as fallback data.
    fn default_request_serializer() -> RequestSerializer<TReq> {
        Arc::new(|request_data| {
            let bytes = request_data.body.as_deref().unwrap_or(&[]);
            serde_json::from_slice::<TReq>(bytes).map_err(|error| {
                let fallback_data = String::from_utf8_lossy(bytes).to_string();
                SerializationError::new(fallback_data, error)
            })
        })
    }

    /// Default serializer that attempts serde JSON deserialization.
    /// On failure, returns a SerializationError with raw bytes as fallback data.
    fn default_response_serializer() -> ResponseSerializer<TRes> {
        Arc::new(|_request_data, response_data| {
            let bytes = response_data.body.as_deref().unwrap_or(&[]);
            serde_json::from_slice::<TRes>(bytes).map_err(|error| {
                let fallback_data = String::from_utf8_lossy(bytes).to_string();
                SerializationError::new(fallback_data, error)
            })
        })
    }

    /// Create a new PostgreSQL handler with a connection pool.
    ///
    /// This will connect to the database but will NOT run migrations.
    /// Use `migrator()` to get a migrator and run migrations separately.
    ///
    /// # Arguments
    ///
    /// * `database_url` - PostgreSQL connection string
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use outlet_postgres::{PostgresHandler, migrator};
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
    ///     // Run migrations first
    ///     let pool = sqlx::PgPool::connect("postgresql://user:pass@localhost/db").await?;
    ///     migrator().run(&pool).await?;
    ///     
    ///     // Create handler
    ///     let handler = PostgresHandler::<MyBodyType, MyBodyType>::new("postgresql://user:pass@localhost/db").await?;
    ///     Ok(())
    /// }
    /// ```
    pub async fn new(database_url: &str) -> Result<Self, PostgresHandlerError> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(PostgresHandlerError::Connection)?;

        Ok(Self {
            pool,
            request_serializer: Self::default_request_serializer(),
            response_serializer: Self::default_response_serializer(),
            path_filter: None,
            instance_id: Uuid::new_v4(),
        })
    }

    /// Add a custom request body serializer.
    ///
    /// The serializer function takes raw bytes and should return a `Result<TReq, String>`.
    /// If the serializer succeeds, the result will be stored as JSONB and `body_parsed` will be true.
    /// If it fails, the raw content will be stored as a UTF-8 string and `body_parsed` will be false.
    ///
    /// # Panics
    ///
    /// This will panic if the serializer succeeds but the resulting `TReq` value cannot be
    /// converted to JSON via `serde_json::to_value()`. This indicates a bug in the `Serialize`
    /// implementation of `TReq` and should be fixed by the caller.
    pub fn with_request_serializer<F>(mut self, serializer: F) -> Self
    where
        F: Fn(&outlet::RequestData) -> Result<TReq, SerializationError> + Send + Sync + 'static,
    {
        self.request_serializer = Arc::new(serializer);
        self
    }

    /// Add a custom response body serializer.
    ///
    /// The serializer function takes raw bytes and should return a `Result<TRes, String>`.
    /// If the serializer succeeds, the result will be stored as JSONB and `body_parsed` will be true.
    /// If it fails, the raw content will be stored as a UTF-8 string and `body_parsed` will be false.
    ///
    /// # Panics
    ///
    /// This will panic if the serializer succeeds but the resulting `TRes` value cannot be
    /// converted to JSON via `serde_json::to_value()`. This indicates a bug in the `Serialize`
    /// implementation of `TRes` and should be fixed by the caller.
    pub fn with_response_serializer<F>(mut self, serializer: F) -> Self
    where
        F: Fn(&outlet::RequestData, &outlet::ResponseData) -> Result<TRes, SerializationError>
            + Send
            + Sync
            + 'static,
    {
        self.response_serializer = Arc::new(serializer);
        self
    }

    /// Add path filtering to only log requests for specific URI prefixes.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use outlet_postgres::{PostgresHandler, PathFilter};
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use serde_json::Value;
    /// let handler = PostgresHandler::<Value, Value>::new("postgresql://user:pass@localhost/db")
    ///     .await?
    ///     .with_path_filter(PathFilter {
    ///         allowed_prefixes: vec!["/api/".to_string(), "/webhook/".to_string()],
    ///         blocked_prefixes: vec!["/api/health".to_string()],
    ///     });
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_path_filter(mut self, filter: PathFilter) -> Self {
        self.path_filter = Some(filter);
        self
    }

    /// Add simple path prefix filtering - only log requests starting with the given prefix.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use outlet_postgres::PostgresHandler;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use serde_json::Value;
    /// let handler = PostgresHandler::<Value, Value>::new("postgresql://user:pass@localhost/db")
    ///     .await?
    ///     .with_path_prefix("/api/");
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_path_prefix(mut self, prefix: &str) -> Self {
        self.path_filter = Some(PathFilter {
            allowed_prefixes: vec![prefix.to_string()],
            blocked_prefixes: vec![],
        });
        self
    }

    /// Create a PostgreSQL handler from an existing connection pool.
    ///
    /// Use this if you already have a connection pool and want to reuse it.
    /// This will NOT run migrations - use `migrator()` to run migrations separately.
    ///
    /// # Arguments
    ///
    /// * `pool` - Existing PostgreSQL connection pool
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use outlet_postgres::{PostgresHandler, migrator};
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
    ///     
    ///     // Run migrations first
    ///     migrator().run(&pool).await?;
    ///     
    ///     // Create handler
    ///     let handler = PostgresHandler::<MyBodyType, MyBodyType>::from_pool(pool).await?;
    ///     Ok(())
    /// }
    /// ```
    pub async fn from_pool(pool: PgPool) -> Result<Self, PostgresHandlerError> {
        Ok(Self {
            pool,
            request_serializer: Self::default_request_serializer(),
            response_serializer: Self::default_response_serializer(),
            path_filter: None,
            instance_id: Uuid::new_v4(),
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

    /// Convert request data to a JSONB value using the configured serializer.
    fn request_body_to_json_with_fallback(
        &self,
        request_data: &outlet::RequestData,
    ) -> (Value, bool) {
        match (self.request_serializer)(request_data) {
            Ok(typed_value) => {
                if let Ok(json_value) = serde_json::to_value(&typed_value) {
                    (json_value, true)
                } else {
                    // This should never happen if the type implements Serialize correctly
                    (
                        Value::String(
                            serde_json::to_string(&typed_value)
                                .expect("Serialized value must be convertible to JSON string"),
                        ),
                        false,
                    )
                }
            }
            Err(serialization_error) => (Value::String(serialization_error.fallback_data), false),
        }
    }

    /// Convert response data to a JSONB value using the configured serializer.
    fn response_body_to_json_with_fallback(
        &self,
        request_data: &outlet::RequestData,
        response_data: &outlet::ResponseData,
    ) -> (Value, bool) {
        match (self.response_serializer)(request_data, response_data) {
            Ok(typed_value) => {
                if let Ok(json_value) = serde_json::to_value(&typed_value) {
                    (json_value, true)
                } else {
                    // This should never happen if the type implements Serialize correctly
                    (
                        Value::String(
                            serde_json::to_string(&typed_value)
                                .expect("Serialized value must be convertible to JSON string"),
                        ),
                        false,
                    )
                }
            }
            Err(serialization_error) => (Value::String(serialization_error.fallback_data), false),
        }
    }

    /// Check if a request URI should be logged based on the configured path filter.
    fn should_log_request(&self, uri: &Uri) -> bool {
        let path = uri.path();
        debug!(%path, "Evaluating prefix");
        let Some(filter) = &self.path_filter else {
            return true; // No filter means log everything
        };

        // Check blocked prefixes first (they take precedence)
        for blocked_prefix in &filter.blocked_prefixes {
            if path.starts_with(blocked_prefix) {
                return false;
            }
        }

        // If no allowed prefixes specified, allow everything (after blocked check)
        if filter.allowed_prefixes.is_empty() {
            return true;
        }

        // Check if URI matches any allowed prefix
        filter
            .allowed_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix))
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
    #[instrument(skip(self, data), fields(correlation_id = %data.correlation_id))]
    async fn handle_request(&self, data: RequestData) {
        // Check if this request should be logged
        if !self.should_log_request(&data.uri) {
            debug!(correlation_id = %data.correlation_id, uri = %data.uri, "Skipping request due to path filter");
            return;
        }

        let headers_json = Self::headers_to_json(&data.headers);
        let (body_json, parsed) = if data.body.is_some() {
            let (json, parsed) = self.request_body_to_json_with_fallback(&data);
            (Some(json), parsed)
        } else {
            (None, false)
        };

        let timestamp: DateTime<Utc> = data.timestamp.into();

        let result = sqlx::query(
            r#"
            INSERT INTO http_requests (instance_id, correlation_id, timestamp, method, uri, headers, body, body_parsed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(self.instance_id)
        .bind(data.correlation_id as i64)
        .bind(timestamp)
        .bind(data.method.to_string())
        .bind(data.uri.to_string())
        .bind(headers_json)
        .bind(body_json)
        .bind(parsed)
        .execute(&self.pool)
        .await;

        if let Err(e) = result {
            error!(correlation_id = %data.correlation_id, error = %e, "Failed to insert request data");
        } else {
            debug!(correlation_id = %data.correlation_id, "Request data inserted successfully");
        }
    }

    #[instrument(skip(self, request_data, response_data), fields(correlation_id = %request_data.correlation_id))]
    async fn handle_response(&self, request_data: RequestData, response_data: ResponseData) {
        let headers_json = Self::headers_to_json(&response_data.headers);
        let (body_json, parsed) = if response_data.body.is_some() {
            let (json, parsed) =
                self.response_body_to_json_with_fallback(&request_data, &response_data);
            (Some(json), parsed)
        } else {
            (None, false)
        };

        let timestamp: DateTime<Utc> = response_data.timestamp.into();
        let duration_ms = response_data.duration.as_millis() as i64;
        let duration_to_first_byte_ms = response_data.duration_to_first_byte.as_millis() as i64;

        let result = sqlx::query(
            r#"
            INSERT INTO http_responses (instance_id, correlation_id, timestamp, status_code, headers, body, body_parsed, duration_to_first_byte_ms, duration_ms)
            SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9
            WHERE EXISTS (SELECT 1 FROM http_requests WHERE instance_id = $1 AND correlation_id = $2)
            "#,
        )
        .bind(self.instance_id)
        .bind(request_data.correlation_id as i64)
        .bind(timestamp)
        .bind(response_data.status.as_u16() as i32)
        .bind(headers_json)
        .bind(body_json)
        .bind(parsed)
        .bind(duration_to_first_byte_ms)
        .bind(duration_ms)
        .execute(&self.pool)
        .await;

        match result {
            Err(e) => {
                error!(correlation_id = %request_data.correlation_id, error = %e, "Failed to insert response data");
            }
            Ok(query_result) => {
                if query_result.rows_affected() > 0 {
                    debug!(correlation_id = %request_data.correlation_id, "Response data inserted successfully");
                } else {
                    debug!(correlation_id = %request_data.correlation_id, "No matching request found for response, skipping insert")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use chrono::{DateTime, Utc};
    use outlet::{RequestData, ResponseData};
    use serde::{Deserialize, Serialize};
    use serde_json::Value;
    use sqlx::PgPool;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    struct TestRequest {
        user_id: u64,
        action: String,
    }

    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    struct TestResponse {
        success: bool,
        message: String,
    }

    fn create_test_request_data() -> RequestData {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), vec!["application/json".into()]);
        headers.insert("user-agent".to_string(), vec!["test-client/1.0".into()]);

        let test_req = TestRequest {
            user_id: 123,
            action: "create_user".to_string(),
        };
        let body = serde_json::to_vec(&test_req).unwrap();

        RequestData {
            method: http::Method::POST,
            uri: http::Uri::from_static("/api/users"),
            headers,
            body: Some(Bytes::from(body)),
            timestamp: SystemTime::now(),
            correlation_id: 0,
        }
    }

    fn create_test_response_data() -> ResponseData {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), vec!["application/json".into()]);

        let test_res = TestResponse {
            success: true,
            message: "User created successfully".to_string(),
        };
        let body = serde_json::to_vec(&test_res).unwrap();

        ResponseData {
            status: http::StatusCode::CREATED,
            headers,
            body: Some(Bytes::from(body)),
            timestamp: SystemTime::now(),
            duration_to_first_byte: Duration::from_millis(100),
            duration: Duration::from_millis(150),
            correlation_id: 0,
        }
    }

    #[sqlx::test]
    async fn test_handler_creation(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<TestRequest, TestResponse>::from_pool(pool.clone())
            .await
            .unwrap();

        // Verify we can get a repository
        let repository = handler.repository();

        // Test initial state - no requests logged yet
        let filter = RequestFilter::default();
        let results = repository.query(filter).await.unwrap();
        assert!(results.is_empty());
    }

    #[sqlx::test]
    async fn test_handle_request_with_typed_body(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<TestRequest, TestResponse>::from_pool(pool.clone())
            .await
            .unwrap();
        let repository = handler.repository();

        let mut request_data = create_test_request_data();
        let correlation_id = 12345;
        request_data.correlation_id = correlation_id;

        // Handle the request
        handler.handle_request(request_data.clone()).await;

        // Query back the request
        let filter = RequestFilter {
            correlation_id: Some(correlation_id as i64),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();

        assert_eq!(results.len(), 1);
        let pair = &results[0];

        assert_eq!(pair.request.correlation_id, correlation_id as i64);
        assert_eq!(pair.request.method, "POST");
        assert_eq!(pair.request.uri, "/api/users");

        // Check that body was parsed successfully
        match &pair.request.body {
            Some(Ok(parsed_body)) => {
                assert_eq!(
                    *parsed_body,
                    TestRequest {
                        user_id: 123,
                        action: "create_user".to_string(),
                    }
                );
            }
            _ => panic!("Expected successfully parsed request body"),
        }

        // Headers should be converted to JSON properly
        let headers_value = &pair.request.headers;
        assert!(headers_value.get("content-type").is_some());
        assert!(headers_value.get("user-agent").is_some());

        // No response yet
        assert!(pair.response.is_none());
    }

    #[sqlx::test]
    async fn test_handle_response_with_typed_body(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<TestRequest, TestResponse>::from_pool(pool.clone())
            .await
            .unwrap();
        let repository = handler.repository();

        let mut request_data = create_test_request_data();
        let mut response_data = create_test_response_data();
        let correlation_id = 54321;
        request_data.correlation_id = correlation_id;
        response_data.correlation_id = correlation_id;

        // Handle both request and response
        handler.handle_request(request_data.clone()).await;
        handler
            .handle_response(request_data, response_data.clone())
            .await;

        // Query back the complete pair
        let filter = RequestFilter {
            correlation_id: Some(correlation_id as i64),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();

        assert_eq!(results.len(), 1);
        let pair = &results[0];

        // Check response data
        let response = pair.response.as_ref().expect("Response should be present");
        assert_eq!(response.correlation_id, correlation_id as i64);
        assert_eq!(response.status_code, 201);
        assert_eq!(response.duration_ms, 150);

        // Check that response body was parsed successfully
        match &response.body {
            Some(Ok(parsed_body)) => {
                assert_eq!(
                    *parsed_body,
                    TestResponse {
                        success: true,
                        message: "User created successfully".to_string(),
                    }
                );
            }
            _ => panic!("Expected successfully parsed response body"),
        }
    }

    #[sqlx::test]
    async fn test_handle_unparseable_body_fallback(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<TestRequest, TestResponse>::from_pool(pool.clone())
            .await
            .unwrap();
        let repository = handler.repository();

        // Create request with invalid JSON for TestRequest
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), vec!["text/plain".into()]);

        let invalid_json_body = b"not valid json for TestRequest";
        let correlation_id = 99999;
        let request_data = RequestData {
            method: http::Method::POST,
            uri: http::Uri::from_static("/api/test"),
            headers,
            body: Some(Bytes::from(invalid_json_body.to_vec())),
            timestamp: SystemTime::now(),
            correlation_id,
        };

        handler.handle_request(request_data).await;

        // Query back and verify fallback to base64
        let filter = RequestFilter {
            correlation_id: Some(correlation_id as i64),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();

        assert_eq!(results.len(), 1);
        let pair = &results[0];

        // Should fallback to raw bytes
        match &pair.request.body {
            Some(Err(raw_bytes)) => {
                assert_eq!(raw_bytes.as_ref(), invalid_json_body);
            }
            _ => panic!("Expected raw bytes fallback for unparseable body"),
        }
    }

    #[sqlx::test]
    async fn test_query_with_multiple_filters(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<Value, Value>::from_pool(pool.clone())
            .await
            .unwrap();
        let repository = handler.repository();

        // Insert multiple requests with different characteristics
        let test_cases = vec![
            (1001, "GET", "/api/users", 200, 100),
            (1002, "POST", "/api/users", 201, 150),
            (1003, "GET", "/api/orders", 404, 50),
            (1004, "PUT", "/api/users/123", 200, 300),
        ];

        for (correlation_id, method, uri, status, duration_ms) in test_cases {
            let mut headers = HashMap::new();
            headers.insert("content-type".to_string(), vec!["application/json".into()]);

            let request_data = RequestData {
                method: method.parse().unwrap(),
                uri: uri.parse().unwrap(),
                headers: headers.clone(),
                body: Some(Bytes::from(b"{}".to_vec())),
                timestamp: SystemTime::now(),
                correlation_id,
            };

            let response_data = ResponseData {
                correlation_id,
                status: http::StatusCode::from_u16(status).unwrap(),
                headers,
                body: Some(Bytes::from(b"{}".to_vec())),
                timestamp: SystemTime::now(),
                duration_to_first_byte: Duration::from_millis(duration_ms / 2),
                duration: Duration::from_millis(duration_ms),
            };

            handler.handle_request(request_data.clone()).await;
            handler.handle_response(request_data, response_data).await;
        }

        // Test method filter
        let filter = RequestFilter {
            method: Some("GET".to_string()),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 2); // 1001, 1003

        // Test status code filter
        let filter = RequestFilter {
            status_code: Some(200),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 2); // 1001, 1004

        // Test URI pattern filter
        let filter = RequestFilter {
            uri_pattern: Some("/api/users%".to_string()),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 3); // 1001, 1002, 1004

        // Test duration range filter
        let filter = RequestFilter {
            min_duration_ms: Some(100),
            max_duration_ms: Some(200),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 2); // 1001, 1002

        // Test combined filters
        let filter = RequestFilter {
            method: Some("GET".to_string()),
            status_code: Some(200),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 1); // Only 1001
        assert_eq!(results[0].request.correlation_id, 1001);
    }

    #[sqlx::test]
    async fn test_query_with_pagination_and_ordering(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<Value, Value>::from_pool(pool.clone())
            .await
            .unwrap();
        let repository = handler.repository();

        // Insert requests with known timestamps
        let now = SystemTime::now();
        for i in 0..5 {
            let correlation_id = 2000 + i;
            let timestamp = now + Duration::from_secs(i * 10); // 10 second intervals

            let mut headers = HashMap::new();
            headers.insert("x-test-id".to_string(), vec![i.to_string().into()]);

            let request_data = RequestData {
                method: http::Method::GET,
                uri: "/api/test".parse().unwrap(),
                headers,
                body: Some(Bytes::from(format!("{{\"id\": {i}}}").into_bytes())),
                timestamp,
                correlation_id,
            };

            handler.handle_request(request_data).await;
        }

        // Test default ordering (ASC) with limit
        let filter = RequestFilter {
            limit: Some(3),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 3);

        // Should be in ascending timestamp order
        for i in 0..2 {
            assert!(results[i].request.timestamp <= results[i + 1].request.timestamp);
        }

        // Test descending order with offset
        let filter = RequestFilter {
            order_by_timestamp_desc: true,
            limit: Some(2),
            offset: Some(1),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 2);

        // Should be in descending order, skipping the first (newest) one
        assert!(results[0].request.timestamp >= results[1].request.timestamp);
    }

    #[sqlx::test]
    async fn test_headers_conversion(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<Value, Value>::from_pool(pool.clone())
            .await
            .unwrap();
        let repository = handler.repository();

        // Test various header scenarios
        let mut headers = HashMap::new();
        headers.insert("single-value".to_string(), vec!["test".into()]);
        headers.insert(
            "multi-value".to_string(),
            vec!["val1".into(), "val2".into()],
        );
        headers.insert("empty-value".to_string(), vec!["".into()]);

        let request_data = RequestData {
            correlation_id: 3000,
            method: http::Method::GET,
            uri: "/test".parse().unwrap(),
            headers,
            body: None,
            timestamp: SystemTime::now(),
        };

        let correlation_id = 3000;
        handler.handle_request(request_data).await;

        let filter = RequestFilter {
            correlation_id: Some(correlation_id as i64),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();

        assert_eq!(results.len(), 1);
        let headers_json = &results[0].request.headers;

        // Single value should be stored as string
        assert_eq!(
            headers_json["single-value"],
            Value::String("test".to_string())
        );

        // Multi-value should be stored as array
        match &headers_json["multi-value"] {
            Value::Array(arr) => {
                assert_eq!(arr.len(), 2);
                assert_eq!(arr[0], Value::String("val1".to_string()));
                assert_eq!(arr[1], Value::String("val2".to_string()));
            }
            _ => panic!("Expected array for multi-value header"),
        }

        // Empty value should still be a string
        assert_eq!(headers_json["empty-value"], Value::String("".to_string()));
    }

    #[sqlx::test]
    async fn test_timestamp_filtering(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<Value, Value>::from_pool(pool.clone())
            .await
            .unwrap();
        let repository = handler.repository();

        let base_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000); // Sept 2020

        // Insert requests at different times
        let times = [
            base_time + Duration::from_secs(0),    // correlation_id 4001
            base_time + Duration::from_secs(3600), // correlation_id 4002 (1 hour later)
            base_time + Duration::from_secs(7200), // correlation_id 4003 (2 hours later)
        ];

        for (i, timestamp) in times.iter().enumerate() {
            let correlation_id = 4001 + i as u64;
            let request_data = RequestData {
                method: http::Method::GET,
                uri: "/test".parse().unwrap(),
                headers: HashMap::new(),
                body: None,
                timestamp: *timestamp,
                correlation_id,
            };

            handler.handle_request(request_data).await;
        }

        // Test timestamp_after filter
        let after_time: DateTime<Utc> = (base_time + Duration::from_secs(1800)).into(); // 30 min after first
        let filter = RequestFilter {
            timestamp_after: Some(after_time),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 2); // Should get 4002 and 4003

        // Test timestamp_before filter
        let before_time: DateTime<Utc> = (base_time + Duration::from_secs(5400)).into(); // 1.5 hours after first
        let filter = RequestFilter {
            timestamp_before: Some(before_time),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 2); // Should get 4001 and 4002

        // Test timestamp range
        let filter = RequestFilter {
            timestamp_after: Some(after_time),
            timestamp_before: Some(before_time),
            ..Default::default()
        };
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 1); // Should get only 4002
        assert_eq!(results[0].request.correlation_id, 4002);
    }

    #[sqlx::test]
    async fn test_path_filtering_allowed_prefix(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<Value, Value>::from_pool(pool.clone())
            .await
            .unwrap()
            .with_path_prefix("/api/");
        let repository = handler.repository();

        // Test requests - some should be logged, some shouldn't
        let test_cases = vec![
            ("/api/users", 1001, true),  // Should be logged
            ("/api/orders", 1002, true), // Should be logged
            ("/health", 1003, false),    // Should NOT be logged
            ("/metrics", 1004, false),   // Should NOT be logged
            ("/api/health", 1005, true), // Should be logged (starts with /api/)
        ];

        for (uri, correlation_id, _should_log) in &test_cases {
            let mut headers = HashMap::new();
            headers.insert("content-type".to_string(), vec!["application/json".into()]);

            let request_data = RequestData {
                method: http::Method::GET,
                uri: uri.parse().unwrap(),
                headers: headers.clone(),
                body: Some(Bytes::from(b"{}".to_vec())),
                timestamp: SystemTime::now(),
                correlation_id: *correlation_id,
            };

            let response_data = ResponseData {
                correlation_id: *correlation_id,
                status: http::StatusCode::OK,
                headers,
                body: Some(Bytes::from(b"{}".to_vec())),
                timestamp: SystemTime::now(),
                duration_to_first_byte: Duration::from_millis(80),
                duration: Duration::from_millis(100),
            };

            handler.handle_request(request_data.clone()).await;
            handler.handle_response(request_data, response_data).await;
        }

        // Query all requests and verify filtering worked
        let filter = RequestFilter::default();
        let results = repository.query(filter).await.unwrap();

        // Should only have logged the /api/ requests (1001, 1002, 1005)
        assert_eq!(results.len(), 3);
        let logged_ids: Vec<i64> = results.iter().map(|r| r.request.correlation_id).collect();
        assert!(logged_ids.contains(&1001));
        assert!(logged_ids.contains(&1002));
        assert!(logged_ids.contains(&1005));
        assert!(!logged_ids.contains(&1003));
        assert!(!logged_ids.contains(&1004));

        // All logged requests should have corresponding responses
        for result in &results {
            assert!(result.response.is_some());
        }
    }

    #[sqlx::test]
    async fn test_path_filtering_blocked_prefix(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        let handler = PostgresHandler::<Value, Value>::from_pool(pool.clone())
            .await
            .unwrap()
            .with_path_filter(PathFilter {
                allowed_prefixes: vec!["/api/".to_string()],
                blocked_prefixes: vec!["/api/health".to_string(), "/api/metrics".to_string()],
            });
        let repository = handler.repository();

        // Test requests
        let test_cases = vec![
            ("http://localhost/api/users", 2001, true), // Should be logged
            ("http://localhost/api/health", 2002, false), // Should be BLOCKED
            ("http://localhost/api/metrics", 2003, false), // Should be BLOCKED
            ("http://localhost/api/orders", 2004, true), // Should be logged
            ("http://localhost/health", 2005, false),   // Not in allowed prefixes
        ];

        for (uri, correlation_id, _should_log) in &test_cases {
            let mut headers = HashMap::new();
            headers.insert("content-type".to_string(), vec!["application/json".into()]);

            let request_data = RequestData {
                method: http::Method::GET,
                uri: uri.parse().unwrap(),
                headers: headers.clone(),
                body: Some(Bytes::from(b"{}".to_vec())),
                timestamp: SystemTime::now(),
                correlation_id: *correlation_id,
            };

            let response_data = ResponseData {
                correlation_id: *correlation_id,
                status: http::StatusCode::OK,
                headers,
                body: Some(Bytes::from(b"{}".to_vec())),
                timestamp: SystemTime::now(),
                duration_to_first_byte: Duration::from_millis(80),
                duration: Duration::from_millis(100),
            };

            handler.handle_request(request_data.clone()).await;
            handler.handle_response(request_data, response_data).await;
        }

        // Should only have logged 2001 and 2004 (blocked prefixes take precedence)
        let filter = RequestFilter::default();
        let results = repository.query(filter).await.unwrap();

        assert_eq!(results.len(), 2);
        let logged_ids: Vec<i64> = results.iter().map(|r| r.request.correlation_id).collect();
        assert!(logged_ids.contains(&2001));
        assert!(logged_ids.contains(&2004));
        assert!(!logged_ids.contains(&2002)); // blocked
        assert!(!logged_ids.contains(&2003)); // blocked
        assert!(!logged_ids.contains(&2005)); // not in allowed prefixes
    }

    #[sqlx::test]
    async fn test_no_path_filtering_logs_everything(pool: PgPool) {
        // Run migrations first
        crate::migrator().run(&pool).await.unwrap();

        // Handler without any path filtering
        let handler = PostgresHandler::<Value, Value>::from_pool(pool.clone())
            .await
            .unwrap();
        let repository = handler.repository();

        let test_uris = vec!["/api/users", "/health", "/metrics", "/random/path"];
        for (i, uri) in test_uris.iter().enumerate() {
            let correlation_id = 3000 + i as u64;
            let mut headers = HashMap::new();
            headers.insert("content-type".to_string(), vec!["application/json".into()]);

            let request_data = RequestData {
                method: http::Method::GET,
                uri: uri.parse().unwrap(),
                headers,
                body: Some(Bytes::from(b"{}".to_vec())),
                timestamp: SystemTime::now(),
                correlation_id,
            };

            handler.handle_request(request_data).await;
        }

        // Should have logged all 4 requests
        let filter = RequestFilter::default();
        let results = repository.query(filter).await.unwrap();
        assert_eq!(results.len(), 4);
    }
}
