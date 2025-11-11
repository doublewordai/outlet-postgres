//! Basic usage example for outlet-postgres.
//!
//! This example demonstrates how to use outlet-postgres to log HTTP requests
//! and responses to PostgreSQL.
//!
//! To run this example:
//! 1. Set up a PostgreSQL database
//! 2. Set the DATABASE_URL environment variable
//! 3. Run: cargo run --example basic_usage

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use outlet::{RequestLoggerConfig, RequestLoggerLayer};
use outlet_postgres::PostgresHandler;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceBuilder;

#[derive(Serialize, Deserialize)]
struct User {
    id: u64,
    name: String,
    email: String,
}

#[derive(Deserialize)]
struct PaginationQuery {
    page: Option<u32>,
    limit: Option<u32>,
}

async fn hello() -> &'static str {
    "Hello, World!"
}

async fn get_user(Path(id): Path<u64>) -> Json<User> {
    Json(User {
        id,
        name: format!("User {id}"),
        email: format!("user{id}@example.com"),
    })
}

async fn create_user(Json(payload): Json<Value>) -> Json<Value> {
    Json(json!({
        "id": 123,
        "message": "User created successfully",
        "data": payload
    }))
}

async fn large_response() -> Json<Value> {
    // Create a large JSON response to test JSONB storage
    let users: Vec<User> = (1..=100)
        .map(|i| User {
            id: i,
            name: format!("User {i}"),
            email: format!("user{i}@example.com"),
        })
        .collect();

    Json(json!({
        "users": users,
        "total": users.len(),
        "metadata": {
            "version": "1.0",
            "generated_at": "2024-01-01T00:00:00Z"
        }
    }))
}

async fn dump_responses(
    State(pool): State<PgPool>,
    Query(pagination): Query<PaginationQuery>,
) -> Json<Value> {
    let page = pagination.page.unwrap_or(1);
    let limit = pagination.limit.unwrap_or(20).min(100); // Max 100 per page
    let offset = (page - 1) * limit;

    // Get total count for pagination metadata
    let total_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM http_requests")
        .fetch_one(&pool)
        .await
        .unwrap_or(0) as u32;

    let total_pages = (total_count + limit - 1) / limit;

    let pairs = sqlx::query_as::<
        _,
        (
            uuid::Uuid,
            uuid::Uuid,
            uuid::Uuid,
            chrono::DateTime<chrono::Utc>,
            String,
            String,
            serde_json::Value,
            serde_json::Value,
            Option<uuid::Uuid>,
            Option<uuid::Uuid>,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<i32>,
            Option<serde_json::Value>,
            Option<serde_json::Value>,
            Option<i64>,
            Option<i64>,
        ),
    >(
        r#"
        SELECT
            req.id,
            req.instance_id,
            req.correlation_id,
            req.timestamp,
            req.method,
            req.uri,
            req.headers,
            req.body,
            resp.id,
            resp.instance_id,
            resp.timestamp,
            resp.status_code,
            resp.headers,
            resp.body,
            resp.duration_to_first_byte_ms,
            resp.duration_ms
        FROM http_requests req
        LEFT JOIN http_responses resp ON req.instance_id = resp.instance_id AND req.correlation_id = resp.correlation_id
        ORDER BY req.timestamp DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(&pool)
    .await;

    match pairs {
        Ok(rows) => {
            let pairs: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    let (
                        request_id,
                        _request_instance_id,
                        correlation_id,
                        request_timestamp,
                        method,
                        uri,
                        request_headers,
                        request_body,
                        response_id,
                        _response_instance_id,
                        response_timestamp,
                        status_code,
                        response_headers,
                        response_body,
                        duration_to_first_byte_ms,
                        duration_ms,
                    ) = row;
                    let response = match response_id {
                        Some(response_id) => json!({
                            "id": response_id,
                            "timestamp": response_timestamp,
                            "status_code": status_code,
                            "headers": response_headers,
                            "body": response_body,
                            "duration_to_first_byte_ms": duration_to_first_byte_ms,
                            "duration_ms": duration_ms
                        }),
                        None => Value::Null,
                    };

                    json!({
                        "correlation_id": correlation_id,
                        "request": {
                            "id": request_id,
                            "timestamp": request_timestamp,
                            "method": method,
                            "uri": uri,
                            "headers": request_headers,
                            "body": request_body
                        },
                        "response": response
                    })
                })
                .collect();

            Json(json!({
                "request_response_pairs": pairs,
                "pagination": {
                    "current_page": page,
                    "per_page": limit,
                    "total_items": total_count,
                    "total_pages": total_pages,
                    "has_next": page < total_pages,
                    "has_prev": page > 1
                },
                "count": pairs.len()
            }))
        }
        Err(e) => Json(json!({
            "error": format!("Failed to fetch request/response pairs: {}", e),
            "request_response_pairs": [],
            "count": 0
        })),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Get database URL from environment
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://postgres:password@localhost/outlet_demo".to_string());

    println!("Connecting to database: {database_url}");

    // Create connection pool
    let pool = PgPool::connect(&database_url).await?;

    // Create PostgreSQL handler from pool
    // Using serde_json::Value as the body type for flexible JSON storage
    let handler = PostgresHandler::<Value>::from_pool(pool.clone()).await?;

    // Configure outlet to capture both request and response bodies
    let config = RequestLoggerConfig {
        capture_request_body: true,
        capture_response_body: true,
        ..Default::default()
    };

    // Create the logging layer
    let layer = RequestLoggerLayer::new(config, handler);

    // Build the application
    let app = Router::new()
        .route("/", get(hello))
        .route("/users/{id}", get(get_user))
        .route("/users", post(create_user))
        .route("/large", get(large_response))
        .route("/dump", get(dump_responses))
        .with_state(pool)
        .layer(ServiceBuilder::new().layer(layer));

    println!("Server starting on http://0.0.0.0:3000");
    println!();
    println!("Try these endpoints:");
    println!("  GET  http://localhost:3000/");
    println!("  GET  http://localhost:3000/users/42");
    println!("  POST http://localhost:3000/users (with JSON body)");
    println!("  GET  http://localhost:3000/large");
    println!("  GET  http://localhost:3000/dump (dumps paginated request/response pairs)");
    println!("      Query params: ?page=1&limit=20 (default: page=1, limit=20, max=100)");
    println!();
    println!("All requests and responses will be logged to PostgreSQL!");

    // Start the server
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;

    Ok(())
}
