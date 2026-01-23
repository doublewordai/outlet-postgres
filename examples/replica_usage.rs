//! Example demonstrating read/write pool separation with DbPools.
//!
//! This example shows how to configure outlet-postgres to use separate
//! connection pools for write operations (logging requests/responses) and
//! read operations (querying logged data).
//!
//! To run this example:
//! 1. Set up PostgreSQL with a primary and replica (or use the same database for both)
//! 2. Set DATABASE_URL and REPLICA_DATABASE_URL environment variables
//! 3. Run: cargo run --example replica_usage

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use outlet::{RequestLoggerConfig, RequestLoggerLayer};
use outlet_postgres::{DbPools, PostgresHandler, RequestFilter};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceBuilder;

#[derive(Serialize, Deserialize)]
struct ApiRequest {
    action: String,
    data: Value,
}

#[derive(Deserialize)]
struct QueryParams {
    limit: Option<i64>,
    method: Option<String>,
}

async fn handle_api_request(Json(payload): Json<ApiRequest>) -> Json<Value> {
    Json(json!({
        "status": "success",
        "action": payload.action,
        "processed": true
    }))
}

async fn get_analytics(
    State(handler): State<PostgresHandler<DbPools, Value, Value>>,
    Query(params): Query<QueryParams>,
) -> Json<Value> {
    let repository = handler.repository();

    let filter = RequestFilter {
        method: params.method,
        limit: params.limit.or(Some(10)),
        order_by_timestamp_desc: true,
        ..Default::default()
    };

    match repository.query(filter).await {
        Ok(pairs) => {
            let results: Vec<Value> = pairs
                .into_iter()
                .map(|pair| {
                    json!({
                        "correlation_id": pair.request.correlation_id,
                        "method": pair.request.method,
                        "uri": pair.request.uri,
                        "timestamp": pair.request.timestamp,
                        "status_code": pair.response.as_ref().map(|r| r.status_code),
                        "duration_ms": pair.response.as_ref().map(|r| r.duration_ms),
                    })
                })
                .collect();

            Json(json!({
                "total": results.len(),
                "requests": results
            }))
        }
        Err(e) => Json(json!({
            "error": e.to_string(),
            "requests": []
        })),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Get database URLs from environment
    let primary_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://postgres:password@localhost/outlet_demo".to_string());

    // For replica, you can use a different URL or the same one for development
    let replica_url = std::env::var("REPLICA_DATABASE_URL").unwrap_or_else(|_| primary_url.clone());

    println!("🔌 Connecting to databases:");
    println!("   Primary (write): {}", primary_url);
    println!("   Replica (read):  {}", replica_url);

    // Create connection pools
    let primary_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&primary_url)
        .await?;

    let replica_pool = PgPoolOptions::new()
        .max_connections(10) // More connections for read-heavy workload
        .connect(&replica_url)
        .await?;

    // Run migrations on the primary database
    outlet_postgres::migrator().run(&primary_pool).await?;

    // Create DbPools with read/write separation
    let pools = if primary_url == replica_url {
        println!("⚠️  Using single pool (primary and replica are the same)");
        DbPools::new(primary_pool)
    } else {
        println!("✓ Using separate pools for read/write separation");
        DbPools::with_replica(primary_pool, replica_pool)
    };

    // Create handler with DbPools
    // Write operations (logging) will use the primary pool
    // Read operations (repository queries) will use the replica pool
    let handler = PostgresHandler::from_pool_provider(pools).await?;

    let config = RequestLoggerConfig {
        capture_request_body: true,
        capture_response_body: true,
        ..Default::default()
    };

    let layer = RequestLoggerLayer::new(config, handler.clone());

    let app = Router::new()
        .route("/api/action", post(handle_api_request))
        .route("/analytics", get(get_analytics))
        .with_state(handler)
        .layer(ServiceBuilder::new().layer(layer));

    println!();
    println!("🚀 Server starting on http://localhost:3000");
    println!();
    println!("📝 Try these commands:");
    println!();
    println!("  # Make some API requests (writes to primary):");
    println!("  curl -X POST http://localhost:3000/api/action \\");
    println!("    -H 'Content-Type: application/json' \\");
    println!("    -d '{{\"action\":\"create\",\"data\":{{\"id\":1}}}}'");
    println!();
    println!("  # Query analytics (reads from replica):");
    println!("  curl 'http://localhost:3000/analytics?limit=10'");
    println!("  curl 'http://localhost:3000/analytics?method=POST&limit=5'");
    println!();
    println!("💡 Benefits of read/write separation:");
    println!("   - Write operations (logging) don't impact read performance");
    println!("   - Read operations (analytics) can scale independently");
    println!("   - Replica can have different connection pool size");
    println!();

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;

    Ok(())
}
