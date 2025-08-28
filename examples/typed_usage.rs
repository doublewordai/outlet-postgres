use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use outlet::{RequestLoggerConfig, RequestLoggerLayer};
use outlet_postgres::{PostgresHandler, RequestFilter, RequestRepository};
use serde::{Deserialize, Serialize};
use tower::ServiceBuilder;

// Define your API request and response types
#[derive(Debug, Deserialize, Serialize, Clone)]
struct CreateUserRequest {
    username: String,
    email: String,
    age: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CreateUserResponse {
    id: u64,
    username: String,
    created: bool,
    message: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ErrorResponse {
    error: String,
    code: u32,
}

// Flexible response type that can capture both success and error responses
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
enum ApiResponse {
    Success(CreateUserResponse),
    Error(ErrorResponse),
}

// Your API handler
async fn create_user(
    Json(payload): Json<CreateUserRequest>,
) -> Result<Json<CreateUserResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Simulate some business logic
    if payload.age < 18 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "User must be 18 or older".to_string(),
                code: 1001,
            }),
        ));
    }

    if payload.username.len() < 3 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Username must be at least 3 characters".to_string(),
                code: 1002,
            }),
        ));
    }

    // Success case
    Ok(Json(CreateUserResponse {
        id: 12345,
        username: payload.username,
        created: true,
        message: "User created successfully".to_string(),
    }))
}

// Application state containing our typed repository
#[derive(Clone)]
struct AppState {
    repository: RequestRepository<CreateUserRequest, ApiResponse>,
}

// Query parameters for the analytics endpoints
#[derive(Deserialize)]
struct QueryParams {
    limit: Option<i64>,
    method: Option<String>,
    min_status: Option<i32>,
    max_status: Option<i32>,
    min_duration: Option<i64>,
}

// Response types for analytics endpoints
#[derive(Serialize)]
struct RequestAnalytics {
    total_requests: usize,
    requests: Vec<RequestSummary>,
}

#[derive(Serialize)]
struct RequestSummary {
    correlation_id: i64,
    method: String,
    uri: String,
    timestamp: String,
    status_code: Option<i32>,
    duration_ms: Option<i64>,
    request_body: Option<CreateUserRequest>,
    response_body: Option<ApiResponse>,
    parsing_info: ParsingInfo,
}

#[derive(Serialize)]
struct ParsingInfo {
    request_parsed: bool,
    response_parsed: bool,
}

// Analytics endpoint to get all requests
async fn get_requests(
    State(state): State<AppState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<RequestAnalytics>, (StatusCode, String)> {
    let filter = RequestFilter {
        method: params.method,
        status_code_min: params.min_status,
        status_code_max: params.max_status,
        min_duration_ms: params.min_duration,
        limit: params.limit,
        order_by_timestamp_desc: true,
        ..Default::default()
    };

    match state.repository.query(filter).await {
        Ok(pairs) => {
            let requests: Vec<RequestSummary> = pairs
                .into_iter()
                .map(|pair| {
                    let (request_body, request_parsed) = match pair.request.body {
                        Some(Ok(body)) => (Some(body), true),
                        _ => (None, false),
                    };

                    let (response_body, response_parsed) =
                        match pair.response.as_ref().and_then(|r| r.body.as_ref()) {
                            Some(Ok(body)) => (Some(body.clone()), true),
                            _ => (None, false),
                        };

                    RequestSummary {
                        correlation_id: pair.request.correlation_id,
                        method: pair.request.method,
                        uri: pair.request.uri,
                        timestamp: pair.request.timestamp.to_rfc3339(),
                        status_code: pair.response.as_ref().map(|r| r.status_code),
                        duration_ms: pair.response.as_ref().map(|r| r.duration_ms),
                        request_body,
                        response_body,
                        parsing_info: ParsingInfo {
                            request_parsed,
                            response_parsed,
                        },
                    }
                })
                .collect();

            Ok(Json(RequestAnalytics {
                total_requests: requests.len(),
                requests,
            }))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// Analytics endpoint to get error responses
async fn get_errors(
    State(state): State<AppState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<RequestAnalytics>, (StatusCode, String)> {
    let filter = RequestFilter {
        method: params.method,
        status_code_min: Some(params.min_status.unwrap_or(400)),
        status_code_max: Some(params.max_status.unwrap_or(599)),
        limit: params.limit,
        order_by_timestamp_desc: true,
        ..Default::default()
    };

    match state.repository.query(filter).await {
        Ok(pairs) => {
            let requests: Vec<RequestSummary> = pairs
                .into_iter()
                .filter(|pair| pair.response.is_some())
                .map(|pair| {
                    let (request_body, request_parsed) = match pair.request.body {
                        Some(Ok(body)) => (Some(body), true),
                        _ => (None, false),
                    };

                    let (response_body, response_parsed) =
                        match pair.response.as_ref().and_then(|r| r.body.as_ref()) {
                            Some(Ok(body)) => (Some(body.clone()), true),
                            _ => (None, false),
                        };

                    RequestSummary {
                        correlation_id: pair.request.correlation_id,
                        method: pair.request.method,
                        uri: pair.request.uri,
                        timestamp: pair.request.timestamp.to_rfc3339(),
                        status_code: pair.response.as_ref().map(|r| r.status_code),
                        duration_ms: pair.response.as_ref().map(|r| r.duration_ms),
                        request_body,
                        response_body,
                        parsing_info: ParsingInfo {
                            request_parsed,
                            response_parsed,
                        },
                    }
                })
                .collect();

            Ok(Json(RequestAnalytics {
                total_requests: requests.len(),
                requests,
            }))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// Analytics endpoint to get slow requests
async fn get_slow_requests(
    State(state): State<AppState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<RequestAnalytics>, (StatusCode, String)> {
    let filter = RequestFilter {
        method: params.method,
        min_duration_ms: Some(params.min_duration.unwrap_or(100)),
        limit: params.limit,
        order_by_timestamp_desc: true,
        ..Default::default()
    };

    match state.repository.query(filter).await {
        Ok(pairs) => {
            let requests: Vec<RequestSummary> = pairs
                .into_iter()
                .filter(|pair| pair.response.is_some())
                .map(|pair| {
                    let (request_body, request_parsed) = match pair.request.body {
                        Some(Ok(body)) => (Some(body), true),
                        _ => (None, false),
                    };

                    let (response_body, response_parsed) =
                        match pair.response.as_ref().and_then(|r| r.body.as_ref()) {
                            Some(Ok(body)) => (Some(body.clone()), true),
                            _ => (None, false),
                        };

                    RequestSummary {
                        correlation_id: pair.request.correlation_id,
                        method: pair.request.method,
                        uri: pair.request.uri,
                        timestamp: pair.request.timestamp.to_rfc3339(),
                        status_code: pair.response.as_ref().map(|r| r.status_code),
                        duration_ms: pair.response.as_ref().map(|r| r.duration_ms),
                        request_body,
                        response_body,
                        parsing_info: ParsingInfo {
                            request_parsed,
                            response_parsed,
                        },
                    }
                })
                .collect();

            Ok(Json(RequestAnalytics {
                total_requests: requests.len(),
                requests,
            }))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgresql://postgres:password@localhost:5432/outlet_demo".to_string()
    });

    println!("Connecting to database: {}", database_url);

    // Create handler with typed request and response bodies
    // ApiResponse uses serde untagged enum to handle both CreateUserResponse and ErrorResponse
    let handler = PostgresHandler::<CreateUserRequest, ApiResponse>::new(&database_url).await?;

    // Get the repository from the handler and store it in app state
    let repository = handler.repository();
    let app_state = AppState { repository };

    let layer = RequestLoggerLayer::new(RequestLoggerConfig::default(), handler);

    let app = Router::new()
        // API routes
        .route("/users", post(create_user))
        // Analytics routes - these use the repository to query logged data
        .route("/analytics/requests", get(get_requests))
        .route("/analytics/errors", get(get_errors))
        .route("/analytics/slow", get(get_slow_requests))
        .with_state(app_state)
        .layer(ServiceBuilder::new().layer(layer));

    println!("🚀 Server starting on http://localhost:3000");
    println!();
    println!("📊 Try making some API requests:");
    println!("  curl -X POST http://localhost:3000/users \\");
    println!("    -H 'Content-Type: application/json' \\");
    println!("    -d '{{\"username\":\"alice\",\"email\":\"alice@example.com\",\"age\":25}}'");
    println!();
    println!("  curl -X POST http://localhost:3000/users \\");
    println!("    -H 'Content-Type: application/json' \\");
    println!("    -d '{{\"username\":\"bob\",\"email\":\"bob@example.com\",\"age\":16}}'");
    println!();
    println!("📈 Then query the analytics endpoints:");
    println!("  curl 'http://localhost:3000/analytics/requests?limit=5'");
    println!("  curl 'http://localhost:3000/analytics/errors?limit=3'");
    println!("  curl 'http://localhost:3000/analytics/slow?min_duration=10&limit=3'");
    println!("  curl 'http://localhost:3000/analytics/requests?method=POST&limit=10'");
    println!();
    println!("💡 The analytics endpoints showcase type-safe querying:");
    println!("   - Request bodies are parsed as CreateUserRequest");
    println!("   - Response bodies are parsed as ApiResponse (Success | Error)");
    println!("   - Raw bytes are preserved when parsing fails");
    println!("   - Serde untagged enum automatically handles different response types");
    println!();

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;

    Ok(())
}
