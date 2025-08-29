use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::de::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, QueryBuilder, Row};

use crate::error::PostgresHandlerError;

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

impl RequestFilter {
    pub fn build_query(&self) -> QueryBuilder<'_, sqlx::Postgres> {
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

        if let Some(correlation_id) = self.correlation_id {
            query.push(" WHERE r.correlation_id = ");
            query.push_bind(correlation_id);
            where_added = true;
        }

        if let Some(method) = &self.method {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("r.method = ");
            query.push_bind(method);
        }

        if let Some(uri_pattern) = &self.uri_pattern {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("r.uri ILIKE ");
            query.push_bind(uri_pattern);
        }

        if let Some(status_code) = self.status_code {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("res.status_code = ");
            query.push_bind(status_code);
        }

        if let Some(min_status) = self.status_code_min {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("res.status_code >= ");
            query.push_bind(min_status);
        }

        if let Some(max_status) = self.status_code_max {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("res.status_code <= ");
            query.push_bind(max_status);
        }

        if let Some(timestamp_after) = self.timestamp_after {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("r.timestamp >= ");
            query.push_bind(timestamp_after);
        }

        if let Some(timestamp_before) = self.timestamp_before {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("r.timestamp <= ");
            query.push_bind(timestamp_before);
        }

        if let Some(min_duration) = self.min_duration_ms {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
                where_added = true;
            }
            query.push("res.duration_ms >= ");
            query.push_bind(min_duration);
        }

        if let Some(max_duration) = self.max_duration_ms {
            if where_added {
                query.push(" AND ");
            } else {
                query.push(" WHERE ");
            }
            query.push("res.duration_ms <= ");
            query.push_bind(max_duration);
        }

        if self.order_by_timestamp_desc {
            query.push(" ORDER BY r.timestamp DESC");
        } else {
            query.push(" ORDER BY r.timestamp ASC");
        }

        if let Some(limit) = self.limit {
            query.push(" LIMIT ");
            query.push_bind(limit);
        }

        if let Some(offset) = self.offset {
            query.push(" OFFSET ");
            query.push_bind(offset);
        }

        query
    }
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
        let rows = filter
            .build_query()
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
                        // Body is stored as UTF-8 string (raw content that failed to parse)
                        if let Value::String(utf8_str) = json_value {
                            Some(Err(Bytes::from(utf8_str.into_bytes())))
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
                                    // Body is stored as UTF-8 string (raw content that failed to parse)
                                    if let Value::String(utf8_str) = json_value {
                                        Some(Err(Bytes::from(utf8_str.into_bytes())))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

    fn validate_sql(sql: &str) -> Result<(), String> {
        let dialect = PostgreSqlDialect {};
        Parser::parse_sql(&dialect, sql)
            .map_err(|e| format!("SQL parse error: {e}"))
            .map(|_| ())
    }

    #[test]
    fn test_default_filter_generates_valid_sql() {
        let filter = RequestFilter::default();
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("ORDER BY r.timestamp ASC"));
        assert!(!sql.contains("WHERE"));
    }

    #[test]
    fn test_correlation_id_filter() {
        let filter = RequestFilter {
            correlation_id: Some(123),
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("WHERE r.correlation_id = $1"));
    }

    #[test]
    fn test_method_filter() {
        let filter = RequestFilter {
            method: Some("POST".to_string()),
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("WHERE r.method = $1"));
    }

    #[test]
    fn test_uri_pattern_filter() {
        let filter = RequestFilter {
            uri_pattern: Some("/api/%".to_string()),
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("WHERE r.uri ILIKE $1"));
    }

    #[test]
    fn test_status_code_exact_filter() {
        let filter = RequestFilter {
            status_code: Some(404),
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("WHERE res.status_code = $1"));
    }

    #[test]
    fn test_status_code_range_filters() {
        let filter = RequestFilter {
            status_code_min: Some(400),
            status_code_max: Some(499),
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("WHERE res.status_code >= $1"));
        assert!(sql.contains("AND res.status_code <= $2"));
    }

    #[test]
    fn test_timestamp_filters() {
        let after = DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let before = DateTime::parse_from_rfc3339("2023-12-31T23:59:59Z")
            .unwrap()
            .with_timezone(&Utc);

        let filter = RequestFilter {
            timestamp_after: Some(after),
            timestamp_before: Some(before),
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("WHERE r.timestamp >= $1"));
        assert!(sql.contains("AND r.timestamp <= $2"));
    }

    #[test]
    fn test_duration_filters() {
        let filter = RequestFilter {
            min_duration_ms: Some(100),
            max_duration_ms: Some(5000),
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("WHERE res.duration_ms >= $1"));
        assert!(sql.contains("AND res.duration_ms <= $2"));
    }

    #[test]
    fn test_ordering_desc() {
        let filter = RequestFilter {
            order_by_timestamp_desc: true,
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("ORDER BY r.timestamp DESC"));
    }

    #[test]
    fn test_ordering_asc() {
        let filter = RequestFilter {
            order_by_timestamp_desc: false,
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("ORDER BY r.timestamp ASC"));
    }

    #[test]
    fn test_pagination() {
        let filter = RequestFilter {
            limit: Some(10),
            offset: Some(20),
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("LIMIT $1"));
        assert!(sql.contains("OFFSET $2"));
    }

    #[test]
    fn test_multiple_filters_use_and() {
        let filter = RequestFilter {
            correlation_id: Some(123),
            method: Some("POST".to_string()),
            status_code: Some(200),
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();
        assert!(sql.contains("WHERE r.correlation_id = $1"));
        assert!(sql.contains("AND r.method = $2"));
        assert!(sql.contains("AND res.status_code = $3"));

        // Should not have multiple WHERE clauses
        assert_eq!(sql.matches("WHERE").count(), 1);
        assert!(sql.matches("AND").count() >= 2);
    }

    #[test]
    fn test_complex_filter_combination() {
        let after = DateTime::parse_from_rfc3339("2023-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let filter = RequestFilter {
            correlation_id: Some(456),
            method: Some("GET".to_string()),
            uri_pattern: Some("/api/users%".to_string()),
            status_code_min: Some(200),
            status_code_max: Some(299),
            timestamp_after: Some(after),
            min_duration_ms: Some(50),
            max_duration_ms: Some(1000),
            limit: Some(100),
            offset: Some(0),
            order_by_timestamp_desc: true,
            ..Default::default()
        };
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();

        // Check all filters are present
        assert!(sql.contains("WHERE r.correlation_id = $1"));
        assert!(sql.contains("AND r.method = $2"));
        assert!(sql.contains("AND r.uri ILIKE $3"));
        assert!(sql.contains("AND res.status_code >= $4"));
        assert!(sql.contains("AND res.status_code <= $5"));
        assert!(sql.contains("AND r.timestamp >= $6"));
        assert!(sql.contains("AND res.duration_ms >= $7"));
        assert!(sql.contains("AND res.duration_ms <= $8"));
        assert!(sql.contains("ORDER BY r.timestamp DESC"));
        assert!(sql.contains("LIMIT $9"));
        assert!(sql.contains("OFFSET $10"));

        // Should have exactly one WHERE
        assert_eq!(sql.matches("WHERE").count(), 1);
    }

    #[test]
    fn test_no_filters_only_has_base_query() {
        let filter = RequestFilter::default();
        let query = filter.build_query();
        let sql = query.sql();

        validate_sql(sql).unwrap();

        // Should contain base SELECT and JOIN
        assert!(sql.contains("SELECT"));
        assert!(sql.contains("FROM http_requests r"));
        assert!(
            sql.contains("LEFT JOIN http_responses res ON r.correlation_id = res.correlation_id")
        );

        // Should not contain WHERE clause
        assert!(!sql.contains("WHERE"));

        // Should have default ordering
        assert!(sql.contains("ORDER BY r.timestamp ASC"));
    }
}
