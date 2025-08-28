# outlet-postgres

PostgreSQL logging handler for the
[outlet](https://github.com/doublewordai/outlet) HTTP request/response
middleware. This crate implements the `RequestHandler` trait from outlet to log
HTTP requests and responses to PostgreSQL with JSONB serialization for bodies.

Features high-performance async logging with automatic table creation and structured query support.

## Quick Start

Add this to your `Cargo.toml`:

```toml
[dependencies]
outlet = "0.1.0"
outlet-postgres = "0.1.0"
axum = "0.8"
tokio = { version = "1.0", features = ["full"] }
tower = "0.5"
```

Basic usage:

```rust
use outlet::{RequestLoggerLayer, RequestLoggerConfig};
use outlet_postgres::PostgresHandler;
use axum::{routing::get, Router};
use tower::ServiceBuilder;

async fn hello() -> &'static str {
    "Hello, World!"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = "postgresql://user:password@localhost/dbname";
    let handler = PostgresHandler::new(database_url).await?;
    let layer = RequestLoggerLayer::new(RequestLoggerConfig::default(), handler);

    let app = Router::new()
        .route("/hello", get(hello))
        .layer(ServiceBuilder::new().layer(layer));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

## Database Schema

The handler automatically creates two tables:

### `http_requests`

- `id` - Primary key
- `correlation_id` - Links to corresponding response
- `timestamp` - When the request was received
- `method` - HTTP method (GET, POST, etc.)
- `uri` - Full request URI
- `headers` - Request headers as JSONB
- `body` - Request body as JSONB (optional)
- `body_parsed` - Whether the body was parsed as the supplied JSON-serde type (default `serde_json::Value`) or not. If not, the `body` field is the base64-encoded binary data.
- `created_at` - When the record was inserted

### `http_responses`

- `id` - Primary key
- `correlation_id` - Links to corresponding request
- `timestamp` - When the response was sent
- `status_code` - HTTP status code
- `headers` - Response headers as JSONB
- `body` - Response body as JSONB (optional)
- `body_parsed` - Whether the body was parsed as the supplied JSON-serde type (default `serde_json::Value`) or not. If not, the `body` field is the base64-encoded binary data.
- `duration_ms` - Request processing time in milliseconds
- `created_at` - When the record was inserted

## Configuration

You can control what data is captured using `RequestLoggerConfig`:

```rust
use outlet::RequestLoggerConfig;

// Capture everything (default)
let config = RequestLoggerConfig::default();

// Only capture requests, not responses
let config = RequestLoggerConfig {
    capture_request_body: true,
    capture_response_body: false,
};

// Headers only, no bodies
let config = RequestLoggerConfig {
    capture_request_body: false,
    capture_response_body: false,
};
```

## Example Queries

Once you're logging requests, you can query the data:

```sql
-- Find all POST requests
SELECT method, uri, timestamp 
FROM http_requests 
WHERE method = 'POST' 
ORDER BY timestamp DESC;

-- Find slow requests (> 1 second)
SELECT r.method, r.uri, s.status_code, s.duration_ms
FROM http_requests r
JOIN http_responses s ON r.correlation_id = s.correlation_id
WHERE s.duration_ms > 1000
ORDER BY s.duration_ms DESC;

-- Search request bodies for specific content
SELECT r.uri, r.body, s.status_code
FROM http_requests r
JOIN http_responses s ON r.correlation_id = s.correlation_id
WHERE r.body @> '{"user_id": 123}';

-- Get response statistics by endpoint
SELECT 
    r.uri,
    COUNT(*) as request_count,
    AVG(s.duration_ms) as avg_duration_ms,
    MIN(s.duration_ms) as min_duration_ms,
    MAX(s.duration_ms) as max_duration_ms
FROM http_requests r
JOIN http_responses s ON r.correlation_id = s.correlation_id
GROUP BY r.uri
ORDER BY request_count DESC;
```

## Running the Example

1. Set up PostgreSQL and create a database
2. Set the `DATABASE_URL` environment variable:

   ```bash
   export DATABASE_URL="postgresql://user:password@localhost/outlet_demo"
   ```

3. Run the example:

   ```bash
   cargo run --example basic_usage
   ```

4. Test the endpoints:

   ```bash
   curl http://localhost:3000/
   curl http://localhost:3000/users/42
   curl -X POST http://localhost:3000/users -H "Content-Type: application/json" -d '{"name":"Alice","email":"alice@example.com"}'
   curl http://localhost:3000/large
   ```

## License

MIT
