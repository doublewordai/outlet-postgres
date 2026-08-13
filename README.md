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
outlet = "0.4.2"
outlet-postgres = "0.4.2"
axum = "0.8"
tokio = { version = "1.0", features = ["full"] }
tower = "0.5"
```

Basic usage:

```rust
use outlet::{RequestLoggerLayer, RequestLoggerConfig};
use outlet_postgres::{CapturePolicy, PostgresHandler};
use axum::{routing::get, Router};
use tower::ServiceBuilder;

async fn hello() -> &'static str {
    "Hello, World!"
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = "postgresql://user:password@localhost/dbname";
    let capture_policy = CapturePolicy::allow_headers(["content-type", "user-agent"])?
        .with_subject_header("x-application-subject")?;
    let handler = PostgresHandler::new(database_url)
        .await?
        .with_capture_policy(capture_policy);
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
- `subject_id` - Optional opaque application subject for targeted lifecycle operations
- `body` - Request body as JSONB (optional)
- `body_parsed` - Whether the body was parsed as the supplied JSON-serde type (default `serde_json::Value`) or not. If not, the `body` field is the base64-encoded binary data.
- `created_at` - When the record was inserted

### `http_responses`

- `id` - Primary key
- `correlation_id` - Links to corresponding request
- `timestamp` - When the response was sent
- `status_code` - HTTP status code
- `headers` - Response headers as JSONB
- `subject_id` - Optional opaque application subject copied from the request
- `body` - Response body as JSONB (optional)
- `body_parsed` - Whether the body was parsed as the supplied JSON-serde type (default `serde_json::Value`) or not. If not, the `body` field is the base64-encoded binary data.
- `duration_ms` - Request processing time in milliseconds
- `created_at` - When the record was inserted

## Configuration

Use `RequestLoggerConfig` to control body capture in the middleware:

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

Use `CapturePolicy` to limit which headers reach serializers and PostgreSQL:

```rust
use outlet_postgres::CapturePolicy;

let capture_policy = CapturePolicy::allow_headers([
    "content-type",
    "user-agent",
])?
.with_subject_header("x-application-subject")?;

let handler = PostgresHandler::new(database_url)
    .await?
    .with_capture_policy(capture_policy);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Prefer an allowlist for sensitive traffic. Header matching is case-insensitive,
and retained names are stored in lowercase. The optional subject value must be
an opaque identifier. Its carrier header is removed unless it is also present
in the request allowlist. The same sanitized data is supplied to custom body
serializers, preventing them from persisting filtered headers inside `body`.

`CapturePolicy::all()` is the backwards-compatible default. The subject columns
are nullable, and the migration intentionally does not create subject indexes.
Existing installations can add suitable indexes separately using their normal
online migration process.

## Retention maintenance

`RetentionRepository` provides bounded building blocks; it does not choose a
retention period or start a scheduler. Applications remain responsible for
leadership, cadence, policy selection, retrying while `has_more` is true, and
recording their own evidence.

Targeted subject deletion writes a one-way subject tombstone before removing
rows. Attributed request and response capture serialize against that tombstone,
so a late in-flight write cannot recreate data after erasure. Tombstones never
contain the original subject value.

```rust
use chrono::{NaiveDate, Utc};
use outlet_postgres::{BatchSize, LogTable, MaintenanceTimeouts, RetentionRepository};

# async fn maintain(pool: sqlx::PgPool, application_cutoff: chrono::DateTime<Utc>) -> Result<(), Box<dyn std::error::Error>> {
let retention = RetentionRepository::new(pool);
let batch = BatchSize::new(1_000)?;

// Run this online before relying on deadline-bound subject erasure. It covers
// existing range and default partitions as well as newly managed partitions.
retention
    .ensure_subject_indexes_concurrently(MaintenanceTimeouts::default())
    .await?;

loop {
    let progress = retention.delete_before_batch(application_cutoff, batch).await?;
    if progress.complete() {
        break;
    }
}

// Pre-create a future UTC day before writes can enter that range.
let future_day = NaiveDate::from_ymd_opt(2030, 1, 1).unwrap();
retention
    .ensure_daily_partitions(future_day, MaintenanceTimeouts::default())
    .await?;

// Inspection uses catalog estimates and an indexed oldest-row lookup rather
// than an exact count of the default partition.
let state = retention
    .inspect_default_partition(LogTable::Requests, application_cutoff)
    .await?;
# Ok(())
# }
```

Subject deletion and time deletion remove request and response pages in one
transaction and return aggregate counts, high-watermarks, and continuation
state. New managed daily partitions receive partial subject indexes. Existing
range and default partitions are covered by the explicit online index
maintenance step above; run it and verify every returned outcome before
enabling deadline-bound subject erasure.

Create future partitions before their lower bound. PostgreSQL will reject a
new range when the default child already contains overlapping rows. Attaching
a range while a default child exists can also lock and scan that child while
PostgreSQL proves the ranges do not overlap. For a large default partition,
drain the target range first and use a deliberately small maintenance timeout;
installations may instead use their normal operator-managed constraint and
partition-attachment process. Use `prune_default_before_batch` to drain
eligible legacy rows in small transactions. `drop_daily_partitions` is intentionally stricter and
irreversible: it only detaches and drops a child whose derived name and catalog
bounds exactly match the requested UTC day and whose upper bound is not newer
than the supplied cutoff.

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
