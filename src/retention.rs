//! Bounded retention and partition-maintenance primitives.
//!
//! This module deliberately does not choose retention durations or run a
//! scheduler. Callers supply cutoffs, batch sizes, and timeout budgets.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use sqlx_pool_router::PoolProvider;
use std::time::Duration;

use crate::PostgresHandlerError;

const MAX_BATCH_SIZE: u32 = 100_000;
const MAX_MAINTENANCE_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Validated upper bound for one maintenance transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatchSize(u32);

impl BatchSize {
    /// Build a non-zero batch size capped to protect transaction and lock time.
    pub fn new(value: u32) -> Result<Self, PostgresHandlerError> {
        if value == 0 || value > MAX_BATCH_SIZE {
            return Err(PostgresHandlerError::InvalidMaintenanceArgument(format!(
                "batch size must be between 1 and {MAX_BATCH_SIZE}"
            )));
        }
        Ok(Self(value))
    }

    fn as_i64(self) -> i64 {
        i64::from(self.0)
    }
}

/// Per-transaction lock and statement timeout budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaintenanceTimeouts {
    lock: Duration,
    statement: Duration,
}

impl MaintenanceTimeouts {
    pub fn new(lock: Duration, statement: Duration) -> Result<Self, PostgresHandlerError> {
        if lock.is_zero()
            || statement.is_zero()
            || lock < Duration::from_millis(1)
            || statement < Duration::from_millis(1)
            || lock > MAX_MAINTENANCE_TIMEOUT
            || statement > MAX_MAINTENANCE_TIMEOUT
        {
            return Err(PostgresHandlerError::InvalidMaintenanceArgument(
                "maintenance timeouts must be between 1 ms and the supported maximum".to_string(),
            ));
        }
        Ok(Self { lock, statement })
    }

    fn lock_millis(self) -> String {
        format!("{}ms", self.lock.as_millis())
    }

    fn statement_millis(self) -> String {
        format!("{}ms", self.statement.as_millis())
    }
}

impl Default for MaintenanceTimeouts {
    fn default() -> Self {
        Self {
            lock: Duration::from_secs(1),
            statement: Duration::from_secs(30),
        }
    }
}

/// Validated settings for one bounded maintenance transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaintenanceOptions {
    pub batch_size: BatchSize,
    pub timeouts: MaintenanceTimeouts,
}

impl MaintenanceOptions {
    pub fn new(batch_size: BatchSize) -> Self {
        Self {
            batch_size,
            timeouts: MaintenanceTimeouts::default(),
        }
    }

    pub fn with_timeouts(mut self, timeouts: MaintenanceTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }
}

impl From<BatchSize> for MaintenanceOptions {
    fn from(batch_size: BatchSize) -> Self {
        Self::new(batch_size)
    }
}

/// Per-table result of a bounded deletion step.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TableDeletionProgress {
    pub deleted: u64,
    pub high_watermark: Option<DateTime<Utc>>,
    pub has_more: bool,
}

/// Aggregate request/response result for one atomic maintenance step.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeletionProgress {
    pub requests: TableDeletionProgress,
    pub responses: TableDeletionProgress,
}

/// Indexed existence result used to verify targeted deletion.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SubjectPresence {
    pub requests: bool,
    pub responses: bool,
}

impl SubjectPresence {
    pub fn is_absent(self) -> bool {
        !self.requests && !self.responses
    }
}

/// One of the two fixed Outlet log tables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogTable {
    Requests,
    Responses,
}

impl LogTable {
    fn name(self) -> &'static str {
        match self {
            Self::Requests => "http_requests",
            Self::Responses => "http_responses",
        }
    }

    fn partition_name(self, day: NaiveDate) -> String {
        format!("{}_p{}", self.name(), day.format("%Y%m%d"))
    }
}

/// Catalog-backed partition metadata. Row counts are PostgreSQL estimates.
#[derive(Clone, Debug, PartialEq)]
pub struct PartitionInfo {
    pub table: LogTable,
    pub schema_name: String,
    pub partition_name: String,
    pub bound_expression: String,
    pub is_default: bool,
    pub lower_bound: Option<DateTime<Utc>>,
    pub upper_bound: Option<DateTime<Utc>>,
    pub estimated_rows: u64,
}

/// Indexed inspection result for the table's resolved default partition.
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultPartitionState {
    pub partition: PartitionInfo,
    pub oldest_timestamp: Option<DateTime<Utc>>,
    pub has_rows_before_cutoff: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnsurePartitionOutcome {
    Created,
    AlreadyExists,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyPartitionOutcome {
    pub day: NaiveDate,
    pub requests: EnsurePartitionOutcome,
    pub responses: EnsurePartitionOutcome,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DropPartitionOutcome {
    Absent,
    Dropped(PartitionInfo),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DailyDropOutcome {
    pub day: NaiveDate,
    pub requests: DropPartitionOutcome,
    pub responses: DropPartitionOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubjectIndexStatus {
    Created,
    AlreadyValid,
    RebuiltInvalid,
}

/// Result of online subject-index maintenance for one existing partition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubjectIndexOutcome {
    pub table: LogTable,
    pub schema_name: String,
    pub partition_name: String,
    pub status: SubjectIndexStatus,
}

impl DeletionProgress {
    pub fn complete(&self) -> bool {
        !self.requests.has_more && !self.responses.has_more
    }
}

/// Maintenance interface backed by the provider's write pool.
#[derive(Clone)]
pub struct RetentionRepository<P: PoolProvider> {
    pool: P,
}

impl<P: PoolProvider> RetentionRepository<P> {
    pub fn new(pool: P) -> Self {
        Self { pool }
    }

    /// Delete one bounded page attributed to an exact opaque subject.
    pub async fn delete_subject_batch<O: Into<MaintenanceOptions>>(
        &self,
        subject_id: &str,
        options: O,
    ) -> Result<DeletionProgress, PostgresHandlerError> {
        if subject_id.is_empty() {
            return Err(PostgresHandlerError::InvalidMaintenanceArgument(
                "subject_id must not be empty".to_string(),
            ));
        }
        let options = options.into();
        let mut transaction = self
            .pool
            .write()
            .begin()
            .await
            .map_err(PostgresHandlerError::Query)?;
        apply_timeouts(&mut transaction, options.timeouts).await?;
        let fingerprint = subject_fingerprint(subject_id);
        // Captures take the shared form of this transaction advisory lock.
        // Once the exclusive lock is acquired, earlier captures have committed;
        // installing the durable tombstone rejects every later capture.
        sqlx::query(
            "WITH subject_lock AS MATERIALIZED ( \
                 SELECT pg_advisory_xact_lock( \
                     hashtextextended('outlet-subject-erasure:' || encode($1, 'hex'), 0) \
                 ) \
             ) \
             INSERT INTO subject_capture_state (subject_fingerprint, erased_at) \
             SELECT $1, NOW() FROM subject_lock \
             ON CONFLICT (subject_fingerprint) DO UPDATE \
             SET erased_at = LEAST(subject_capture_state.erased_at, EXCLUDED.erased_at)",
        )
        .bind(&fingerprint)
        .execute(&mut *transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;
        let responses = delete_subject_table(
            &mut transaction,
            LogTable::Responses,
            subject_id,
            options.batch_size,
        )
        .await?;
        let requests = delete_subject_table(
            &mut transaction,
            LogTable::Requests,
            subject_id,
            options.batch_size,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(PostgresHandlerError::Query)?;
        Ok(DeletionProgress {
            requests,
            responses,
        })
    }

    /// Return whether future capture for an exact subject is blocked.
    pub async fn is_subject_blocked(&self, subject_id: &str) -> Result<bool, PostgresHandlerError> {
        if subject_id.is_empty() {
            return Err(PostgresHandlerError::InvalidMaintenanceArgument(
                "subject_id must not be empty".to_string(),
            ));
        }
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM subject_capture_state \
             WHERE subject_fingerprint = $1 AND erased_at IS NOT NULL)",
        )
        .bind(subject_fingerprint(subject_id))
        .fetch_one(self.pool.write())
        .await
        .map_err(PostgresHandlerError::Query)
    }

    /// Verify whether either table still contains an exact opaque subject.
    ///
    /// This intentionally reads the primary for read-after-delete consistency.
    pub async fn subject_presence(
        &self,
        subject_id: &str,
    ) -> Result<SubjectPresence, PostgresHandlerError> {
        if subject_id.is_empty() {
            return Err(PostgresHandlerError::InvalidMaintenanceArgument(
                "subject_id must not be empty".to_string(),
            ));
        }
        let row = sqlx::query(
            "SELECT \
                EXISTS(SELECT 1 FROM http_requests WHERE subject_id = $1 LIMIT 1) AS requests, \
                EXISTS(SELECT 1 FROM http_responses WHERE subject_id = $1 LIMIT 1) AS responses",
        )
        .bind(subject_id)
        .fetch_one(self.pool.write())
        .await
        .map_err(PostgresHandlerError::Query)?;
        Ok(SubjectPresence {
            requests: row.get("requests"),
            responses: row.get("responses"),
        })
    }

    /// Delete one bounded page older than an application-supplied cutoff.
    pub async fn delete_before_batch<O: Into<MaintenanceOptions>>(
        &self,
        cutoff: DateTime<Utc>,
        options: O,
    ) -> Result<DeletionProgress, PostgresHandlerError> {
        let options = options.into();
        let mut transaction = self
            .pool
            .write()
            .begin()
            .await
            .map_err(PostgresHandlerError::Query)?;
        apply_timeouts(&mut transaction, options.timeouts).await?;
        let responses = delete_before_table(
            &mut transaction,
            LogTable::Responses,
            cutoff,
            options.batch_size,
        )
        .await?;
        let requests = delete_before_table(
            &mut transaction,
            LogTable::Requests,
            cutoff,
            options.batch_size,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(PostgresHandlerError::Query)?;
        Ok(DeletionProgress {
            requests,
            responses,
        })
    }

    /// Inspect catalog metadata for all children of one log table.
    pub async fn list_partitions(
        &self,
        table: LogTable,
    ) -> Result<Vec<PartitionInfo>, PostgresHandlerError> {
        list_partitions_on(self.pool.read(), table).await
    }

    /// Inspect the resolved default child without an exact row count scan.
    pub async fn inspect_default_partition(
        &self,
        table: LogTable,
        cutoff: DateTime<Utc>,
    ) -> Result<DefaultPartitionState, PostgresHandlerError> {
        let partition = self
            .list_partitions(table)
            .await?
            .into_iter()
            .find(|partition| partition.is_default)
            .ok_or_else(|| {
                PostgresHandlerError::UnsafePartitionOperation(format!(
                    "{} has no default partition",
                    table.name()
                ))
            })?;
        let qualified = qualified_identifier(&partition.schema_name, &partition.partition_name);
        let oldest_sql =
            format!("SELECT timestamp FROM ONLY {qualified} ORDER BY timestamp, id LIMIT 1");
        let oldest_timestamp = sqlx::query_scalar(&oldest_sql)
            .fetch_optional(self.pool.read())
            .await
            .map_err(PostgresHandlerError::Query)?;
        let eligible_sql =
            format!("SELECT EXISTS(SELECT 1 FROM ONLY {qualified} WHERE timestamp < $1 LIMIT 1)");
        let has_rows_before_cutoff = sqlx::query_scalar(&eligible_sql)
            .bind(cutoff)
            .fetch_one(self.pool.read())
            .await
            .map_err(PostgresHandlerError::Query)?;
        Ok(DefaultPartitionState {
            partition,
            oldest_timestamp,
            has_rows_before_cutoff,
        })
    }

    /// Delete bounded pages only from the catalog-resolved default children.
    pub async fn prune_default_before_batch<O: Into<MaintenanceOptions>>(
        &self,
        cutoff: DateTime<Utc>,
        options: O,
    ) -> Result<DeletionProgress, PostgresHandlerError> {
        let options = options.into();
        let mut transaction = self
            .pool
            .write()
            .begin()
            .await
            .map_err(PostgresHandlerError::Query)?;
        apply_timeouts(&mut transaction, options.timeouts).await?;
        let response_partition =
            default_partition_on(&mut transaction, LogTable::Responses).await?;
        let request_partition = default_partition_on(&mut transaction, LogTable::Requests).await?;
        let responses = delete_before_named_table(
            &mut transaction,
            &qualified_identifier(
                &response_partition.schema_name,
                &response_partition.partition_name,
            ),
            cutoff,
            options.batch_size,
        )
        .await?;
        let requests = delete_before_named_table(
            &mut transaction,
            &qualified_identifier(
                &request_partition.schema_name,
                &request_partition.partition_name,
            ),
            cutoff,
            options.batch_size,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(PostgresHandlerError::Query)?;
        Ok(DeletionProgress {
            requests,
            responses,
        })
    }

    /// Ensure one UTC daily child exists for both request and response logs.
    pub async fn ensure_daily_partitions(
        &self,
        day: NaiveDate,
        timeouts: MaintenanceTimeouts,
    ) -> Result<DailyPartitionOutcome, PostgresHandlerError> {
        let mut transaction = self
            .pool
            .write()
            .begin()
            .await
            .map_err(PostgresHandlerError::Query)?;
        apply_timeouts(&mut transaction, timeouts).await?;
        lock_partition_maintenance(&mut transaction).await?;
        let requests = ensure_daily_partition(&mut transaction, LogTable::Requests, day).await?;
        let responses = ensure_daily_partition(&mut transaction, LogTable::Responses, day).await?;
        transaction
            .commit()
            .await
            .map_err(PostgresHandlerError::Query)?;
        Ok(DailyPartitionOutcome {
            day,
            requests,
            responses,
        })
    }

    /// Detach and drop catalog-proven expired daily children atomically.
    pub async fn drop_daily_partitions(
        &self,
        day: NaiveDate,
        cutoff: DateTime<Utc>,
        timeouts: MaintenanceTimeouts,
    ) -> Result<DailyDropOutcome, PostgresHandlerError> {
        let mut transaction = self
            .pool
            .write()
            .begin()
            .await
            .map_err(PostgresHandlerError::Query)?;
        apply_timeouts(&mut transaction, timeouts).await?;
        lock_partition_maintenance(&mut transaction).await?;
        let requests =
            drop_daily_partition(&mut transaction, LogTable::Requests, day, cutoff).await?;
        let responses =
            drop_daily_partition(&mut transaction, LogTable::Responses, day, cutoff).await?;
        transaction
            .commit()
            .await
            .map_err(PostgresHandlerError::Query)?;
        Ok(DailyDropOutcome {
            day,
            requests,
            responses,
        })
    }

    /// Build or verify the subject-deletion index on every existing request
    /// and response partition without blocking ordinary reads and writes.
    ///
    /// PostgreSQL does not support `CREATE INDEX CONCURRENTLY` on the
    /// partitioned parent. This deliberately visits each catalog-resolved
    /// child outside a transaction, repairs an interrupted invalid build, and
    /// verifies the exact index shape before reporting success. A session
    /// advisory lock serializes concurrent maintenance callers.
    pub async fn ensure_subject_indexes_concurrently(
        &self,
        timeouts: MaintenanceTimeouts,
    ) -> Result<Vec<SubjectIndexOutcome>, PostgresHandlerError> {
        let mut connection = self
            .pool
            .write()
            .acquire()
            .await
            .map_err(PostgresHandlerError::Query)?;
        // Session advisory locks and GUCs must never leak back into the pool if
        // this cancellation-sensitive future is dropped during a concurrent
        // index build. The dedicated backend is cheap relative to this rare
        // maintenance operation and closes on every exit path.
        connection.close_on_drop();
        sqlx::query(
            "SELECT set_config('lock_timeout', $1, false), \
                    set_config('statement_timeout', $2, false)",
        )
        .bind(timeouts.lock_millis())
        .bind(timeouts.statement_millis())
        .execute(&mut *connection)
        .await
        .map_err(PostgresHandlerError::Query)?;
        // Timeouts must be installed by a completed command before starting
        // the potentially blocking advisory-lock command.
        sqlx::query(
            "SELECT pg_advisory_lock(hashtext('outlet-postgres:subject-index-maintenance'))",
        )
        .execute(&mut *connection)
        .await
        .map_err(PostgresHandlerError::Query)?;

        let operation = async {
            let mut outcomes = Vec::new();
            for table in [LogTable::Requests, LogTable::Responses] {
                let rows = sqlx::query(
                    "SELECT child_namespace.nspname AS schema_name, \
                            child.relname AS partition_name, \
                            pg_get_expr(child.relpartbound, child.oid) AS bound_expression, \
                            greatest(child.reltuples, 0)::bigint AS estimated_rows \
                     FROM pg_inherits inheritance \
                     JOIN pg_class child ON child.oid = inheritance.inhrelid \
                     JOIN pg_namespace child_namespace ON child_namespace.oid = child.relnamespace \
                     WHERE inheritance.inhparent = to_regclass($1) \
                     ORDER BY child_namespace.nspname, child.relname",
                )
                .bind(table.name())
                .fetch_all(&mut *connection)
                .await
                .map_err(PostgresHandlerError::Query)?;

                for row in rows {
                    let partition = partition_from_row(table, row);
                    let index_name = subject_index_name(&partition.partition_name);
                    let existing: Option<(bool, bool)> = sqlx::query_as(
                        "SELECT index.indisvalid, \
                                index.indnkeyatts = 3 \
                                AND pg_get_indexdef(index.indexrelid, 1, true) = 'subject_id' \
                                AND pg_get_indexdef(index.indexrelid, 2, true) \
                                    IN ('timestamp', '\"timestamp\"') \
                                AND pg_get_indexdef(index.indexrelid, 3, true) = 'id' \
                                AND pg_get_expr(index.indpred, index.indrelid) \
                                    IN ('(subject_id IS NOT NULL)', 'subject_id IS NOT NULL') \
                         FROM pg_class index_class \
                         JOIN pg_namespace namespace ON namespace.oid = index_class.relnamespace \
                         JOIN pg_index index ON index.indexrelid = index_class.oid \
                         WHERE namespace.nspname = $1 AND index_class.relname = $2 \
                           AND index.indrelid = to_regclass($3)",
                    )
                    .bind(&partition.schema_name)
                    .bind(&index_name)
                    .bind(qualified_identifier(
                        &partition.schema_name,
                        &partition.partition_name,
                    ))
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(PostgresHandlerError::Query)?;

                    let rebuilt = match existing {
                        Some((true, true)) => {
                            outcomes.push(SubjectIndexOutcome {
                                table,
                                schema_name: partition.schema_name,
                                partition_name: partition.partition_name,
                                status: SubjectIndexStatus::AlreadyValid,
                            });
                            continue;
                        }
                        Some((_, false)) => {
                            return Err(PostgresHandlerError::UnsafePartitionOperation(format!(
                                "index {index_name} exists with an unexpected definition"
                            )));
                        }
                        Some((false, true)) => {
                            let qualified_index =
                                qualified_identifier(&partition.schema_name, &index_name);
                            sqlx::query(&format!("DROP INDEX CONCURRENTLY {qualified_index}"))
                                .execute(&mut *connection)
                                .await
                                .map_err(PostgresHandlerError::Query)?;
                            true
                        }
                        None => false,
                    };

                    let qualified_table =
                        qualified_identifier(&partition.schema_name, &partition.partition_name);
                    let quoted_index = quote_identifier(&index_name);
                    sqlx::query(&format!(
                        "CREATE INDEX CONCURRENTLY {quoted_index} ON {qualified_table} \
                         (subject_id, timestamp, id) WHERE subject_id IS NOT NULL"
                    ))
                    .execute(&mut *connection)
                    .await
                    .map_err(PostgresHandlerError::Query)?;
                    outcomes.push(SubjectIndexOutcome {
                        table,
                        schema_name: partition.schema_name,
                        partition_name: partition.partition_name,
                        status: if rebuilt {
                            SubjectIndexStatus::RebuiltInvalid
                        } else {
                            SubjectIndexStatus::Created
                        },
                    });
                }
            }
            Ok(outcomes)
        }
        .await;

        let cleanup = sqlx::query(
            "SELECT \
                 pg_advisory_unlock(hashtext('outlet-postgres:subject-index-maintenance')), \
                 set_config('lock_timeout', '0', false), \
                 set_config('statement_timeout', '0', false)",
        )
        .execute(&mut *connection)
        .await
        .map_err(PostgresHandlerError::Query);
        match operation {
            Ok(outcomes) => {
                cleanup?;
                Ok(outcomes)
            }
            Err(error) => Err(error),
        }
    }
}

fn subject_index_name(partition_name: &str) -> String {
    let candidate = format!("{partition_name}_subject_id_idx");
    if candidate.len() <= 63 {
        return candidate;
    }
    let digest = format!("{:x}", Sha256::digest(candidate.as_bytes()));
    let prefix: String = partition_name.chars().take(40).collect();
    format!("{prefix}_subject_{}", &digest[..12])
}

pub(crate) fn subject_fingerprint(subject_id: &str) -> Vec<u8> {
    Sha256::digest(subject_id.as_bytes()).to_vec()
}

async fn apply_timeouts(
    transaction: &mut Transaction<'_, Postgres>,
    timeouts: MaintenanceTimeouts,
) -> Result<(), PostgresHandlerError> {
    sqlx::query(
        "SELECT set_config('lock_timeout', $1, true), \
         set_config('statement_timeout', $2, true)",
    )
    .bind(timeouts.lock_millis())
    .bind(timeouts.statement_millis())
    .execute(&mut **transaction)
    .await
    .map_err(PostgresHandlerError::Query)?;
    Ok(())
}

async fn lock_partition_maintenance(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), PostgresHandlerError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('outlet-postgres:partition-maintenance'))")
        .execute(&mut **transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;
    Ok(())
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn qualified_identifier(schema: &str, relation: &str) -> String {
    format!(
        "{}.{}",
        quote_identifier(schema),
        quote_identifier(relation)
    )
}

fn day_bounds(day: NaiveDate) -> Result<(DateTime<Utc>, DateTime<Utc>), PostgresHandlerError> {
    let next_day = day.succ_opt().ok_or_else(|| {
        PostgresHandlerError::InvalidMaintenanceArgument(
            "daily partition date has no following day".to_string(),
        )
    })?;
    Ok((
        Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).expect("midnight is valid")),
        Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).expect("midnight is valid")),
    ))
}

fn parse_catalog_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f%#z")
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn parse_partition_bounds(expression: &str) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let Some(inner) = expression
        .strip_prefix("FOR VALUES FROM ('")
        .and_then(|value| value.strip_suffix("')"))
    else {
        return (None, None);
    };
    let Some((lower, upper)) = inner.split_once("') TO ('") else {
        return (None, None);
    };
    (
        parse_catalog_timestamp(lower),
        parse_catalog_timestamp(upper),
    )
}

fn partition_from_row(table: LogTable, row: sqlx::postgres::PgRow) -> PartitionInfo {
    let bound_expression: String = row.get("bound_expression");
    let (lower_bound, upper_bound) = parse_partition_bounds(&bound_expression);
    let estimated_rows: i64 = row.get("estimated_rows");
    PartitionInfo {
        table,
        schema_name: row.get("schema_name"),
        partition_name: row.get("partition_name"),
        is_default: bound_expression == "DEFAULT",
        bound_expression,
        lower_bound,
        upper_bound,
        estimated_rows: estimated_rows.max(0) as u64,
    }
}

async fn list_partitions_on(
    pool: &sqlx::PgPool,
    table: LogTable,
) -> Result<Vec<PartitionInfo>, PostgresHandlerError> {
    let rows = sqlx::query(
        "SELECT child_namespace.nspname AS schema_name, \
                child.relname AS partition_name, \
                pg_get_expr(child.relpartbound, child.oid) AS bound_expression, \
                greatest(child.reltuples, 0)::bigint AS estimated_rows \
         FROM pg_inherits inheritance \
         JOIN pg_class child ON child.oid = inheritance.inhrelid \
         JOIN pg_namespace child_namespace ON child_namespace.oid = child.relnamespace \
         WHERE inheritance.inhparent = to_regclass($1) \
         ORDER BY child_namespace.nspname, child.relname",
    )
    .bind(table.name())
    .fetch_all(pool)
    .await
    .map_err(PostgresHandlerError::Query)?;
    Ok(rows
        .into_iter()
        .map(|row| partition_from_row(table, row))
        .collect())
}

async fn list_partitions_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    table: LogTable,
) -> Result<Vec<PartitionInfo>, PostgresHandlerError> {
    let rows = sqlx::query(
        "SELECT child_namespace.nspname AS schema_name, \
                child.relname AS partition_name, \
                pg_get_expr(child.relpartbound, child.oid) AS bound_expression, \
                greatest(child.reltuples, 0)::bigint AS estimated_rows \
         FROM pg_inherits inheritance \
         JOIN pg_class child ON child.oid = inheritance.inhrelid \
         JOIN pg_namespace child_namespace ON child_namespace.oid = child.relnamespace \
         WHERE inheritance.inhparent = to_regclass($1) \
         ORDER BY child_namespace.nspname, child.relname",
    )
    .bind(table.name())
    .fetch_all(&mut **transaction)
    .await
    .map_err(PostgresHandlerError::Query)?;
    Ok(rows
        .into_iter()
        .map(|row| partition_from_row(table, row))
        .collect())
}

async fn default_partition_on(
    transaction: &mut Transaction<'_, Postgres>,
    table: LogTable,
) -> Result<PartitionInfo, PostgresHandlerError> {
    list_partitions_in_transaction(transaction, table)
        .await?
        .into_iter()
        .find(|partition| partition.is_default)
        .ok_or_else(|| {
            PostgresHandlerError::UnsafePartitionOperation(format!(
                "{} has no default partition",
                table.name()
            ))
        })
}

async fn parent_schema_on(
    transaction: &mut Transaction<'_, Postgres>,
    table: LogTable,
) -> Result<String, PostgresHandlerError> {
    sqlx::query_scalar(
        "SELECT namespace.nspname \
         FROM pg_class parent \
         JOIN pg_namespace namespace ON namespace.oid = parent.relnamespace \
         WHERE parent.oid = to_regclass($1)",
    )
    .bind(table.name())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(PostgresHandlerError::Query)?
    .ok_or_else(|| {
        PostgresHandlerError::UnsafePartitionOperation(format!(
            "{} is not visible on the current search path",
            table.name()
        ))
    })
}

async fn ensure_daily_partition(
    transaction: &mut Transaction<'_, Postgres>,
    table: LogTable,
    day: NaiveDate,
) -> Result<EnsurePartitionOutcome, PostgresHandlerError> {
    let (lower, upper) = day_bounds(day)?;
    let partition_name = table.partition_name(day);
    if let Some(existing) = list_partitions_in_transaction(transaction, table)
        .await?
        .into_iter()
        .find(|partition| partition.partition_name == partition_name)
    {
        if !existing.is_default
            && existing.lower_bound == Some(lower)
            && existing.upper_bound == Some(upper)
        {
            return Ok(EnsurePartitionOutcome::AlreadyExists);
        }
        return Err(PostgresHandlerError::UnsafePartitionOperation(format!(
            "partition {partition_name} exists with unexpected bounds {}",
            existing.bound_expression
        )));
    }

    let schema = parent_schema_on(transaction, table).await?;
    let parent = qualified_identifier(&schema, table.name());
    let child = qualified_identifier(&schema, &partition_name);
    let create_sql = format!(
        "CREATE TABLE {child} PARTITION OF {parent} \
         FOR VALUES FROM ('{}') TO ('{}')",
        lower.to_rfc3339(),
        upper.to_rfc3339()
    );
    sqlx::query(&create_sql)
        .execute(&mut **transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;

    // PostgreSQL creates the index in the table's schema and does not accept a
    // schema-qualified index name in CREATE INDEX.
    let subject_index = quote_identifier(&subject_index_name(&partition_name));
    let index_sql = format!(
        "CREATE INDEX {subject_index} ON {child} (subject_id, timestamp, id) \
         WHERE subject_id IS NOT NULL"
    );
    sqlx::query(&index_sql)
        .execute(&mut **transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;
    Ok(EnsurePartitionOutcome::Created)
}

async fn drop_daily_partition(
    transaction: &mut Transaction<'_, Postgres>,
    table: LogTable,
    day: NaiveDate,
    cutoff: DateTime<Utc>,
) -> Result<DropPartitionOutcome, PostgresHandlerError> {
    let partition_name = table.partition_name(day);
    let Some(partition) = list_partitions_in_transaction(transaction, table)
        .await?
        .into_iter()
        .find(|partition| partition.partition_name == partition_name)
    else {
        return Ok(DropPartitionOutcome::Absent);
    };
    let (expected_lower, expected_upper) = day_bounds(day)?;
    if partition.is_default
        || partition.lower_bound != Some(expected_lower)
        || partition.upper_bound != Some(expected_upper)
        || expected_upper > cutoff
    {
        return Err(PostgresHandlerError::UnsafePartitionOperation(format!(
            "partition {partition_name} is not a fully expired managed daily partition"
        )));
    }

    let parent_schema = parent_schema_on(transaction, table).await?;
    let parent = qualified_identifier(&parent_schema, table.name());
    let child = qualified_identifier(&partition.schema_name, &partition.partition_name);
    let detach_sql = format!("ALTER TABLE {parent} DETACH PARTITION {child}");
    sqlx::query(&detach_sql)
        .execute(&mut **transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;
    let drop_sql = format!("DROP TABLE {child}");
    sqlx::query(&drop_sql)
        .execute(&mut **transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;
    Ok(DropPartitionOutcome::Dropped(partition))
}

fn progress_from_row(row: &sqlx::postgres::PgRow, has_more: bool) -> TableDeletionProgress {
    let deleted: i64 = row.get("deleted");
    TableDeletionProgress {
        deleted: deleted.try_into().expect("COUNT(*) is non-negative"),
        high_watermark: row.get("high_watermark"),
        has_more,
    }
}

async fn delete_subject_table(
    transaction: &mut Transaction<'_, Postgres>,
    table: LogTable,
    subject_id: &str,
    batch_size: BatchSize,
) -> Result<TableDeletionProgress, PostgresHandlerError> {
    let table = table.name();
    let delete_sql = format!(
        "WITH candidates AS (\
             SELECT id, timestamp FROM {table} \
             WHERE subject_id = $1 \
             ORDER BY timestamp, id LIMIT $2 FOR UPDATE SKIP LOCKED\
         ), deleted AS (\
             DELETE FROM {table} target USING candidates \
             WHERE target.id = candidates.id AND target.timestamp = candidates.timestamp \
             RETURNING target.timestamp\
         ) \
         SELECT count(*)::bigint AS deleted, max(timestamp) AS high_watermark FROM deleted"
    );
    let row = sqlx::query(&delete_sql)
        .bind(subject_id)
        .bind(batch_size.as_i64())
        .fetch_one(&mut **transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;
    let exists_sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE subject_id = $1 LIMIT 1)");
    let has_more: bool = sqlx::query_scalar(&exists_sql)
        .bind(subject_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;
    Ok(progress_from_row(&row, has_more))
}

async fn delete_before_table(
    transaction: &mut Transaction<'_, Postgres>,
    table: LogTable,
    cutoff: DateTime<Utc>,
    batch_size: BatchSize,
) -> Result<TableDeletionProgress, PostgresHandlerError> {
    delete_before_relation(transaction, table.name(), false, cutoff, batch_size).await
}

async fn delete_before_named_table(
    transaction: &mut Transaction<'_, Postgres>,
    qualified_table: &str,
    cutoff: DateTime<Utc>,
    batch_size: BatchSize,
) -> Result<TableDeletionProgress, PostgresHandlerError> {
    delete_before_relation(transaction, qualified_table, true, cutoff, batch_size).await
}

async fn delete_before_relation(
    transaction: &mut Transaction<'_, Postgres>,
    table: &str,
    only: bool,
    cutoff: DateTime<Utc>,
    batch_size: BatchSize,
) -> Result<TableDeletionProgress, PostgresHandlerError> {
    let relation = if only {
        format!("ONLY {table}")
    } else {
        table.to_string()
    };
    let delete_sql = format!(
        "WITH candidates AS (\
             SELECT id, timestamp FROM {relation} \
             WHERE timestamp < $1 \
             ORDER BY timestamp, id LIMIT $2 FOR UPDATE SKIP LOCKED\
         ), deleted AS (\
             DELETE FROM {relation} target USING candidates \
             WHERE target.id = candidates.id AND target.timestamp = candidates.timestamp \
             RETURNING target.timestamp\
         ) \
         SELECT count(*)::bigint AS deleted, max(timestamp) AS high_watermark FROM deleted"
    );
    let row = sqlx::query(&delete_sql)
        .bind(cutoff)
        .bind(batch_size.as_i64())
        .fetch_one(&mut **transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;
    let exists_sql =
        format!("SELECT EXISTS(SELECT 1 FROM {relation} WHERE timestamp < $1 LIMIT 1)");
    let has_more: bool = sqlx::query_scalar(&exists_sql)
        .bind(cutoff)
        .fetch_one(&mut **transaction)
        .await
        .map_err(PostgresHandlerError::Query)?;
    Ok(progress_from_row(&row, has_more))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration as ChronoDuration, TimeZone};
    use serde_json::json;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;

    async fn insert_pair(
        pool: &PgPool,
        correlation_id: i64,
        timestamp: DateTime<Utc>,
        subject: &str,
    ) {
        let instance_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO http_requests \
             (instance_id, correlation_id, timestamp, method, uri, headers, body, body_parsed, subject_id) \
             VALUES ($1, $2, $3, 'POST', '/example', $4, $5, true, $6)",
        )
        .bind(instance_id)
        .bind(correlation_id)
        .bind(timestamp)
        .bind(json!({}))
        .bind(json!({"request": correlation_id}))
        .bind(subject)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO http_responses \
             (instance_id, correlation_id, timestamp, status_code, headers, body, body_parsed, duration_to_first_byte_ms, duration_ms, subject_id) \
             VALUES ($1, $2, $3, 200, $4, $5, true, 1, 2, $6)",
        )
        .bind(instance_id)
        .bind(correlation_id)
        .bind(timestamp + ChronoDuration::milliseconds(1))
        .bind(json!({}))
        .bind(json!({"response": correlation_id}))
        .bind(subject)
        .execute(pool)
        .await
        .unwrap();
    }

    #[test]
    fn batch_size_is_bounded() {
        assert!(BatchSize::new(0).is_err());
        assert!(BatchSize::new(1).is_ok());
        assert!(BatchSize::new(MAX_BATCH_SIZE).is_ok());
        assert!(BatchSize::new(MAX_BATCH_SIZE + 1).is_err());
    }

    #[test]
    fn maintenance_timeouts_are_bounded() {
        assert!(MaintenanceTimeouts::new(Duration::ZERO, Duration::from_secs(1)).is_err());
        assert!(MaintenanceTimeouts::new(Duration::from_secs(1), Duration::ZERO).is_err());
        assert!(MaintenanceTimeouts::new(Duration::from_nanos(1), Duration::from_secs(1)).is_err());
        assert!(
            MaintenanceTimeouts::new(Duration::from_secs(1), Duration::from_micros(999)).is_err()
        );
        assert!(
            MaintenanceTimeouts::new(Duration::from_millis(1), Duration::from_millis(1)).is_ok()
        );
        assert!(MaintenanceTimeouts::new(Duration::from_secs(1), MAX_MAINTENANCE_TIMEOUT).is_ok());
        assert!(MaintenanceTimeouts::new(
            Duration::from_secs(1),
            MAX_MAINTENANCE_TIMEOUT + Duration::from_millis(1),
        )
        .is_err());
    }

    #[sqlx::test]
    async fn subject_deletion_is_bounded_isolated_and_idempotent(pool: PgPool) {
        crate::migrator().run(&pool).await.unwrap();
        let base = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        for id in 1..=3 {
            insert_pair(&pool, id, base + ChronoDuration::seconds(id), "subject-a").await;
        }
        insert_pair(&pool, 99, base, "subject-b").await;
        let repository = RetentionRepository::new(pool.clone());

        let first = repository
            .delete_subject_batch("subject-a", BatchSize::new(2).unwrap())
            .await
            .unwrap();
        assert_eq!(first.requests.deleted, 2);
        assert_eq!(first.responses.deleted, 2);
        assert!(first.requests.has_more);
        assert!(first.responses.has_more);

        let second = repository
            .delete_subject_batch("subject-a", BatchSize::new(2).unwrap())
            .await
            .unwrap();
        assert_eq!(second.requests.deleted, 1);
        assert_eq!(second.responses.deleted, 1);
        assert!(second.complete());

        let repeated = repository
            .delete_subject_batch("subject-a", BatchSize::new(2).unwrap())
            .await
            .unwrap();
        assert_eq!(repeated, DeletionProgress::default());

        let other_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM http_requests WHERE subject_id = 'subject-b'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(other_count, 1);
        assert!(repository
            .subject_presence("subject-a")
            .await
            .unwrap()
            .is_absent());
        assert_eq!(
            repository.subject_presence("subject-b").await.unwrap(),
            SubjectPresence {
                requests: true,
                responses: true,
            }
        );
        assert!(repository.is_subject_blocked("subject-a").await.unwrap());
        assert!(!repository.is_subject_blocked("subject-b").await.unwrap());
    }

    #[sqlx::test]
    async fn cutoff_deletion_uses_strict_boundary_and_reports_high_watermark(pool: PgPool) {
        crate::migrator().run(&pool).await.unwrap();
        let cutoff = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        insert_pair(&pool, 1, cutoff - ChronoDuration::seconds(2), "old").await;
        insert_pair(&pool, 2, cutoff - ChronoDuration::seconds(1), "old").await;
        insert_pair(&pool, 3, cutoff, "boundary").await;
        let repository = RetentionRepository::new(pool.clone());

        let progress = repository
            .delete_before_batch(cutoff, BatchSize::new(1).unwrap())
            .await
            .unwrap();
        assert_eq!(progress.requests.deleted, 1);
        assert_eq!(progress.responses.deleted, 1);
        assert!(progress.requests.high_watermark.is_some());
        assert!(progress.requests.has_more);

        repository
            .delete_before_batch(cutoff, BatchSize::new(10).unwrap())
            .await
            .unwrap();
        let boundary_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM http_requests WHERE subject_id = 'boundary'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(boundary_count, 1);
    }

    #[test]
    fn catalog_range_bounds_parse_with_timezone_offsets() {
        let (lower, upper) = parse_partition_bounds(
            "FOR VALUES FROM ('2026-08-13 01:00:00+01') TO ('2026-08-14 01:00:00+01')",
        );
        assert_eq!(
            lower,
            Some(Utc.with_ymd_and_hms(2026, 8, 13, 0, 0, 0).unwrap())
        );
        assert_eq!(
            upper,
            Some(Utc.with_ymd_and_hms(2026, 8, 14, 0, 0, 0).unwrap())
        );
        assert_eq!(parse_partition_bounds("DEFAULT"), (None, None));
    }

    #[sqlx::test]
    async fn daily_partitions_are_idempotent_indexed_and_route_new_rows(pool: PgPool) {
        crate::migrator().run(&pool).await.unwrap();
        let repository = RetentionRepository::new(pool.clone());
        let day = NaiveDate::from_ymd_opt(2030, 4, 5).unwrap();

        let created = repository
            .ensure_daily_partitions(day, MaintenanceTimeouts::default())
            .await
            .unwrap();
        assert_eq!(created.requests, EnsurePartitionOutcome::Created);
        assert_eq!(created.responses, EnsurePartitionOutcome::Created);

        let repeated = repository
            .ensure_daily_partitions(day, MaintenanceTimeouts::default())
            .await
            .unwrap();
        assert_eq!(repeated.requests, EnsurePartitionOutcome::AlreadyExists);
        assert_eq!(repeated.responses, EnsurePartitionOutcome::AlreadyExists);

        let timestamp = Utc.with_ymd_and_hms(2030, 4, 5, 12, 0, 0).unwrap();
        insert_pair(&pool, 500, timestamp, "partition-subject").await;
        let request_partition: String = sqlx::query_scalar(
            "SELECT tableoid::regclass::text FROM http_requests WHERE correlation_id = 500",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let response_partition: String = sqlx::query_scalar(
            "SELECT tableoid::regclass::text FROM http_responses WHERE correlation_id = 500",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(request_partition.ends_with("http_requests_p20300405"));
        assert!(response_partition.ends_with("http_responses_p20300405"));

        let request_info = repository
            .list_partitions(LogTable::Requests)
            .await
            .unwrap()
            .into_iter()
            .find(|partition| partition.partition_name == "http_requests_p20300405")
            .unwrap();
        let (lower, upper) = day_bounds(day).unwrap();
        assert_eq!(request_info.lower_bound, Some(lower));
        assert_eq!(request_info.upper_bound, Some(upper));

        let subject_indexes: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_indexes \
             WHERE tablename IN ('http_requests_p20300405', 'http_responses_p20300405') \
               AND indexdef LIKE '%(subject_id, \"timestamp\", id)%'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(subject_indexes, 2);
    }

    #[sqlx::test]
    async fn default_partition_inspection_and_pruning_are_bounded(pool: PgPool) {
        crate::migrator().run(&pool).await.unwrap();
        let repository = RetentionRepository::new(pool.clone());
        let cutoff = Utc.with_ymd_and_hms(2029, 1, 1, 0, 0, 0).unwrap();
        insert_pair(
            &pool,
            600,
            cutoff - ChronoDuration::seconds(2),
            "default-old",
        )
        .await;
        insert_pair(&pool, 601, cutoff, "default-boundary").await;

        let state = repository
            .inspect_default_partition(LogTable::Requests, cutoff)
            .await
            .unwrap();
        assert!(state.partition.is_default);
        assert_eq!(
            state.oldest_timestamp,
            Some(cutoff - ChronoDuration::seconds(2))
        );
        assert!(state.has_rows_before_cutoff);

        let progress = repository
            .prune_default_before_batch(cutoff, BatchSize::new(1).unwrap())
            .await
            .unwrap();
        assert_eq!(progress.requests.deleted, 1);
        assert_eq!(progress.responses.deleted, 1);
        assert!(progress.complete());

        let boundary_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM ONLY http_requests_default WHERE subject_id = 'default-boundary'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(boundary_count, 1);
    }

    #[sqlx::test]
    async fn online_subject_index_maintenance_covers_existing_partitions(pool: PgPool) {
        crate::migrator().run(&pool).await.unwrap();
        let repository = RetentionRepository::new(pool.clone());
        let day = NaiveDate::from_ymd_opt(2030, 8, 9).unwrap();
        repository
            .ensure_daily_partitions(day, MaintenanceTimeouts::default())
            .await
            .unwrap();
        for table in [LogTable::Requests, LogTable::Responses] {
            for partition in repository.list_partitions(table).await.unwrap() {
                let index = qualified_identifier(
                    &partition.schema_name,
                    &subject_index_name(&partition.partition_name),
                );
                sqlx::query(&format!("DROP INDEX IF EXISTS {index}"))
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }

        let outcomes = repository
            .ensure_subject_indexes_concurrently(MaintenanceTimeouts::default())
            .await
            .unwrap();
        assert_eq!(outcomes.len(), 4, "default and managed child per table");
        assert!(outcomes
            .iter()
            .all(|outcome| outcome.status == SubjectIndexStatus::Created));

        let repeated = repository
            .ensure_subject_indexes_concurrently(MaintenanceTimeouts::default())
            .await
            .unwrap();
        assert!(repeated
            .iter()
            .all(|outcome| outcome.status == SubjectIndexStatus::AlreadyValid));
    }

    #[sqlx::test]
    async fn daily_drop_requires_catalog_proof_of_full_expiry_and_is_idempotent(pool: PgPool) {
        crate::migrator().run(&pool).await.unwrap();
        let repository = RetentionRepository::new(pool.clone());
        let day = NaiveDate::from_ymd_opt(2031, 5, 6).unwrap();
        repository
            .ensure_daily_partitions(day, MaintenanceTimeouts::default())
            .await
            .unwrap();
        let (_, upper) = day_bounds(day).unwrap();

        let error = repository
            .drop_daily_partitions(
                day,
                upper - ChronoDuration::milliseconds(1),
                MaintenanceTimeouts::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            PostgresHandlerError::UnsafePartitionOperation(_)
        ));

        let dropped = repository
            .drop_daily_partitions(day, upper, MaintenanceTimeouts::default())
            .await
            .unwrap();
        assert!(matches!(dropped.requests, DropPartitionOutcome::Dropped(_)));
        assert!(matches!(
            dropped.responses,
            DropPartitionOutcome::Dropped(_)
        ));

        let repeated = repository
            .drop_daily_partitions(day, upper, MaintenanceTimeouts::default())
            .await
            .unwrap();
        assert_eq!(repeated.requests, DropPartitionOutcome::Absent);
        assert_eq!(repeated.responses, DropPartitionOutcome::Absent);
        assert!(repository
            .list_partitions(LogTable::Requests)
            .await
            .unwrap()
            .iter()
            .any(|partition| partition.is_default));
    }

    #[sqlx::test]
    async fn daily_partition_creation_rejects_a_conflicting_managed_name(pool: PgPool) {
        crate::migrator().run(&pool).await.unwrap();
        sqlx::query(
            "CREATE TABLE http_requests_p20320607 PARTITION OF http_requests \
             FOR VALUES FROM ('2032-06-08T00:00:00Z') TO ('2032-06-09T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let repository = RetentionRepository::new(pool);

        let error = repository
            .ensure_daily_partitions(
                NaiveDate::from_ymd_opt(2032, 6, 7).unwrap(),
                MaintenanceTimeouts::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            PostgresHandlerError::UnsafePartitionOperation(_)
        ));
    }

    #[sqlx::test]
    async fn maintenance_writes_use_the_write_pool(pool: PgPool) {
        crate::migrator().run(&pool).await.unwrap();
        insert_pair(
            &pool,
            700,
            Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap(),
            "write-pool-subject",
        )
        .await;
        let pools = crate::TestDbPools::new(pool).await.unwrap();
        let repository = RetentionRepository::new(pools);

        let progress = repository
            .delete_subject_batch("write-pool-subject", BatchSize::new(10).unwrap())
            .await
            .unwrap();
        assert_eq!(progress.requests.deleted, 1);
        assert_eq!(progress.responses.deleted, 1);
    }

    #[sqlx::test]
    async fn concurrent_workers_skip_locked_rows_without_double_deletion(pool: PgPool) {
        crate::migrator().run(&pool).await.unwrap();
        let base = Utc.with_ymd_and_hms(2028, 1, 1, 0, 0, 0).unwrap();
        for id in 1..=6 {
            insert_pair(
                &pool,
                800 + id,
                base + ChronoDuration::seconds(id),
                "concurrent-subject",
            )
            .await;
        }
        let first = RetentionRepository::new(pool.clone());
        let second = RetentionRepository::new(pool.clone());

        let (first_progress, second_progress) = tokio::join!(
            first.delete_subject_batch("concurrent-subject", BatchSize::new(4).unwrap()),
            second.delete_subject_batch("concurrent-subject", BatchSize::new(4).unwrap())
        );
        let first_progress = first_progress.unwrap();
        let second_progress = second_progress.unwrap();
        assert_eq!(
            first_progress.requests.deleted + second_progress.requests.deleted,
            6
        );
        assert_eq!(
            first_progress.responses.deleted + second_progress.responses.deleted,
            6
        );

        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM http_requests WHERE subject_id = 'concurrent-subject'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
    }
}
