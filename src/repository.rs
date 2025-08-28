use base64::Engine;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::de::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool, QueryBuilder, Row};

use crate::error::PostgresHandlerError;

#[derive(Debug, FromRow)]
struct HttpRequestRow {
    pub id: i64,
    pub correlation_id: i64,
    pub timestamp: DateTime<Utc>,
    pub method: String,
    pub uri: String,
    pub headers: Value,
    pub body: Option<Value>,
    pub body_parsed: Option<bool>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct HttpResponseRow {
    pub id: i64,
    pub correlation_id: i64,
    pub timestamp: DateTime<Utc>,
    pub status_code: i32,
    pub headers: Value,
    pub body: Option<Value>,
    pub body_parsed: Option<bool>,
    pub duration_ms: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct HttpRequest<TReq> {
    pub id: i64,
    pub correlation_id: i64,
    pub timestamp: DateTime<Utc>,
    pub method: String,
    pub uri: String,
    pub headers: Value,
    pub body: Option<Result<TReq, Bytes>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct HttpResponse<TRes> {
    pub id: i64,
    pub correlation_id: i64,
    pub timestamp: DateTime<Utc>,
    pub status_code: i32,
    pub headers: Value,
    pub body: Option<Result<TRes, Bytes>>,
    pub duration_ms: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct RequestResponsePair<TReq, TRes> {
    pub request: HttpRequest<TReq>,
    pub response: Option<HttpResponse<TRes>>,
}

#[derive(Debug, Default)]
pub struct RequestFilter {
    pub correlation_id: Option<i64>,
    pub method: Option<String>,
    pub uri_pattern: Option<String>,
    pub status_code: Option<i32>,
    pub status_code_min: Option<i32>,
    pub status_code_max: Option<i32>,
    pub timestamp_after: Option<DateTime<Utc>>,
    pub timestamp_before: Option<DateTime<Utc>>,
    pub min_duration_ms: Option<i64>,
    pub max_duration_ms: Option<i64>,
    pub body_parsed: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub order_by_timestamp_desc: bool,
}

#[derive(Clone)]
pub struct RequestRepository<TReq, TRes> {
    pool: PgPool,
    _phantom_req: std::marker::PhantomData<TReq>,
    _phantom_res: std::marker::PhantomData<TRes>,
}

impl<TReq, TRes> RequestRepository<TReq, TRes>
where
    TReq: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
    TRes: for<'de> Deserialize<'de> + Serialize + Send + Sync + 'static,
{
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            _phantom_req: std::marker::PhantomData,
            _phantom_res: std::marker::PhantomData,
        }
    }

    pub async fn query(
        &self,
        filter: RequestFilter,
    ) -> Result<Vec<RequestResponsePair<TReq, TRes>>, PostgresHandlerError> {
        let mut query = QueryBuilder::new(
            r#"
            SELECT 
                r.id as req_id, r.correlation_id as req_correlation_id, r.timestamp as req_timestamp, 
                r.method, r.uri, r.headers as req_headers, r.body as req_body, r.body_parsed as req_body_parsed, r.created_at as req_created_at,
                res.id as res_id, res.correlation_id as res_correlation_id, res.timestamp as res_timestamp,
                res.status_code, res.headers as res_headers, res.body as res_body, res.body_parsed as res_body_parsed, res.duration_ms, res.created_at as res_created_at
            FROM http_requests r
            LEFT JOIN http_responses res ON r.correlation_id = res.correlation_id
            "#,
        );

        let mut where_added = false;

        if let Some(correlation_id) = filter.correlation_id {
            query.push(" WHERE r.correlation_id = ");
            query.push_bind(correlation_id);
            where_added = true;
        }

        if let Some(method) = &filter.method {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("r.method = ");
            query.push_bind(method);
        }

        if let Some(uri_pattern) = &filter.uri_pattern {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("r.uri ILIKE ");
            query.push_bind(uri_pattern);
        }

        if let Some(status_code) = filter.status_code {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("res.status_code = ");
            query.push_bind(status_code);
        }

        if let Some(min_status) = filter.status_code_min {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("res.status_code >= ");
            query.push_bind(min_status);
        }

        if let Some(max_status) = filter.status_code_max {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("res.status_code <= ");
            query.push_bind(max_status);
        }

        if let Some(timestamp_after) = filter.timestamp_after {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("r.timestamp >= ");
            query.push_bind(timestamp_after);
        }

        if let Some(timestamp_before) = filter.timestamp_before {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("r.timestamp <= ");
            query.push_bind(timestamp_before);
        }

        if let Some(min_duration) = filter.min_duration_ms {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("res.duration_ms >= ");
            query.push_bind(min_duration);
        }

        if let Some(max_duration) = filter.max_duration_ms {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("res.duration_ms <= ");
            query.push_bind(max_duration);
        }

        if filter.order_by_timestamp_desc {
            query.push(" ORDER BY r.timestamp DESC");
        } else {
            query.push(" ORDER BY r.timestamp ASC");
        }

        if let Some(limit) = filter.limit {
            query.push(" LIMIT ");
            query.push_bind(limit);
        }

        if let Some(offset) = filter.offset {
            query.push(" OFFSET ");
            query.push_bind(offset);
        }

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(PostgresHandlerError::Query)?;

        let mut pairs = Vec::new();
        for row in rows {
            let req_body = row.try_get::<Option<Value>, _>("req_body").unwrap_or(None);
            let req_body_parsed = row
                .try_get::<Option<bool>, _>("req_body_parsed")
                .unwrap_or(Some(false));

            let request_body = match req_body {
                Some(json_value) => {
                    if req_body_parsed == Some(true) {
                        // Body was successfully parsed as TReq when stored
                        Some(Ok(serde_json::from_value::<TReq>(json_value)
                            .map_err(PostgresHandlerError::Json)?))
                    } else {
                        // Body is stored as base64 string, decode it
                        if let Value::String(base64_str) = json_value {
                            let decoded_bytes = base64::engine::general_purpose::STANDARD
                                .decode(&base64_str)
                                .map_err(|_| {
                                    PostgresHandlerError::Json(Error::custom(
                                        "Failed to decode base64",
                                    ))
                                })?;
                            Some(Err(Bytes::from(decoded_bytes)))
                        } else {
                            return Err(PostgresHandlerError::Json(Error::custom(
                                "Invalid body format",
                            )));
                        }
                    }
                }
                None => None,
            };

            let request = HttpRequest {
                id: row.get("req_id"),
                correlation_id: row.get("req_correlation_id"),
                timestamp: row.get("req_timestamp"),
                method: row.get("method"),
                uri: row.get("uri"),
                headers: row.get("req_headers"),
                body: request_body,
                created_at: row.get("req_created_at"),
            };

            let response = if let Ok(res_id) = row.try_get::<Option<i64>, _>("res_id") {
                res_id
                    .map(|_| -> Result<HttpResponse<TRes>, PostgresHandlerError> {
                        let res_body = row.try_get::<Option<Value>, _>("res_body").unwrap_or(None);
                        let res_body_parsed = row
                            .try_get::<Option<bool>, _>("res_body_parsed")
                            .unwrap_or(Some(false));

                        let response_body = match res_body {
                            Some(json_value) => {
                                if res_body_parsed == Some(true) {
                                    // Body was successfully parsed as TRes when stored
                                    Some(Ok(serde_json::from_value::<TRes>(json_value)
                                        .map_err(PostgresHandlerError::Json)?))
                                } else {
                                    // Body is stored as base64 string, decode it
                                    if let Value::String(base64_str) = json_value {
                                        let decoded_bytes =
                                            base64::engine::general_purpose::STANDARD
                                                .decode(&base64_str)
                                                .map_err(|_| {
                                                    PostgresHandlerError::Json(Error::custom(
                                                        "Failed to decode base64",
                                                    ))
                                                })?;
                                        Some(Err(Bytes::from(decoded_bytes)))
                                    } else {
                                        return Err(PostgresHandlerError::Json(Error::custom(
                                            "Invalid body format",
                                        )));
                                    }
                                }
                            }
                            None => None,
                        };

                        Ok(HttpResponse {
                            id: row.get("res_id"),
                            correlation_id: row.get("res_correlation_id"),
                            timestamp: row.get("res_timestamp"),
                            status_code: row.get("status_code"),
                            headers: row.get("res_headers"),
                            body: response_body,
                            duration_ms: row.get("duration_ms"),
                            created_at: row.get("res_created_at"),
                        })
                    })
                    .transpose()?
            } else {
                None
            };

            pairs.push(RequestResponsePair { request, response });
        }

        Ok(pairs)
    }

    pub async fn count_requests(&self) -> Result<i64, PostgresHandlerError> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM http_requests")
            .fetch_one(&self.pool)
            .await
            .map_err(PostgresHandlerError::Query)?;

        Ok(count.0)
    }

    pub async fn count_responses(&self) -> Result<i64, PostgresHandlerError> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM http_responses")
            .fetch_one(&self.pool)
            .await
            .map_err(PostgresHandlerError::Query)?;

        Ok(count.0)
    }
}
