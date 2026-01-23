# outlet-postgres

PostgreSQL logging handler for the [outlet](https://github.com/doublewordai/outlet) HTTP request/response middleware. This crate implements the `RequestHandler` trait to log HTTP requests and responses to PostgreSQL with JSONB serialization for bodies.

## Project Overview

### What This Does

- **PostgreSQL Integration**: Async PostgreSQL operations via sqlx for logging HTTP traffic
- **JSONB Bodies**: Serializes request/response bodies to JSONB fields for flexible querying
- **Type-safe Querying**: Query logged data with typed request/response bodies
- **Read/Write Pool Separation**: Optional primary/replica routing via `sqlx-pool-router` for scalability
- **Correlation**: Links requests and responses via correlation IDs
- **Flexible Serialization**: Custom serializers with graceful fallback for unparseable bodies

### Architecture

```
┌──────────────────┐
│ Axum Middleware  │
│   (outlet)       │
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│ PostgresHandler  │
│ (this crate)     │
└────────┬─────────┘
         │
    ┌────┴────┐
    ▼         ▼
┌────────┐  ┌─────────┐
│Primary │  │ Replica │ (optional, via DbPools)
│(write) │  │ (read)  │
└────────┘  └─────────┘
```

### Key Components

- **`PostgresHandler<P, TReq, TRes>`**: Main handler implementing `outlet::RequestHandler`
  - Generic over pool provider `P` (supports `PgPool` or `DbPools`)
  - Generic over request/response body types for type-safe JSONB storage
  - Writes to primary via `.write()`, reads via `.read()`

- **`RequestRepository<P, TReq, TRes>`**: Query interface for logged data
  - Supports filtering by method, status code, URI pattern, timestamps, duration, etc.
  - Always uses read pool for analytics queries

- **`DbPools` & `TestDbPools`** (from `sqlx-pool-router`):
  - `DbPools`: Production read/write separation
  - `TestDbPools`: Test helper with read-only replica enforcement

## Development Workflow

### Branch Strategy

**Work on feature branches, squash merge to main.**

- Main branch: `main`
- Feature branches: Use descriptive names (`add-replica-tests`, `fix-serialization`, etc.)
- Commits on feature branches do NOT need to be backwards compatible or individually buildable
- Squash merge when merging to `main` to maintain clean history

### Commit Standards

**Use [Conventional Commits](https://www.conventionalcommits.org/) format:**

```
<type>: <description>

[optional body]

[optional footer]
```

**Types:**
- `feat:` - New feature
- `fix:` - Bug fix
- `docs:` - Documentation only
- `test:` - Adding or updating tests
- `refactor:` - Code change that neither fixes a bug nor adds a feature
- `chore:` - Maintenance tasks (deps, tooling, etc.)

**Examples:**
```
feat: add comprehensive test coverage for read/write pool separation

fix: handle null values in response body serialization

docs: update README with DbPools usage examples

test: add integration tests for typed body serialization
```

**Footer:**
Always include Claude attribution in commits:
```
Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>
```

### Testing Requirements

**All changes must pass:**

1. **Tests**: `cargo test`
   - Unit tests in `src/lib.rs` and `src/repository.rs`
   - Integration tests using `#[sqlx::test]` macro
   - Doc tests in code examples

2. **Linting**: `cargo clippy --all-targets --all-features -- -D warnings`
   - Zero warnings required
   - Fix all clippy suggestions

3. **Formatting**: `cargo fmt`
   - Auto-format before committing
   - Check with `cargo fmt --check`

**Test Coverage Philosophy:**
- Write tests for all public APIs
- Use `TestDbPools` to verify read/write pool separation
- Test both success and error paths
- Test serialization with typed bodies and fallback behavior

### Database Configuration

Tests require PostgreSQL to be running locally.

**Setup `.env` file:**
```bash
DATABASE_URL="postgresql://your_user@localhost/postgres"
```

The `#[sqlx::test]` macro automatically:
- Creates temporary test databases
- Runs migrations
- Cleans up after tests

### Read/Write Pool Separation

**Critical Implementation Details:**

All database operations MUST use the correct pool via `PoolProvider` trait:

- **Write operations** → `.write()` (primary pool)
  - `handle_request()` - Logging incoming requests
  - `handle_response()` - Logging responses

- **Read operations** → `.read()` (replica pool)
  - `repository.query()` - Querying logged data

**Testing Read/Write Separation:**

Use `TestDbPools` to enforce proper pool usage:

```rust
#[sqlx::test]
async fn test_writes_use_write_pool(pool: PgPool) {
    let test_pools = TestDbPools::new(pool).await.unwrap();
    let handler = PostgresHandler::from_pool_provider(test_pools).await.unwrap();

    // This succeeds because it uses .write()
    handler.handle_request(request_data).await;

    // This would FAIL if accidentally using .read()
    // because TestDbPools enforces read-only on replica
}
```

`TestDbPools` creates a read-only replica connection, so any write operations to `.read()` will fail with a PostgreSQL read-only transaction error.

## Code Organization

### File Structure

```
outlet-postgres/
├── src/
│   ├── lib.rs           # PostgresHandler, serialization, tests
│   ├── repository.rs    # RequestRepository, filtering, query tests
│   └── error.rs         # Error types
├── examples/
│   ├── basic_usage.rs   # Simple PgPool example
│   ├── typed_usage.rs   # Custom body types example
│   └── replica_usage.rs # DbPools read/write separation example
├── migrations/          # SQLx migrations for http_requests/http_responses tables
└── tests/              # (currently empty, tests in src/)
```

### Important Patterns

**Generic Pool Provider Pattern:**

All handler and repository types are generic over `P: PoolProvider`:

```rust
pub struct PostgresHandler<P = PgPool, TReq = Value, TRes = Value>
where
    P: PoolProvider,
    TReq: Serialize + Deserialize,
    TRes: Serialize + Deserialize,
{
    pool: P,
    // ...
}
```

This allows:
- Development: `PostgresHandler<PgPool>` (single pool)
- Production: `PostgresHandler<DbPools>` (primary/replica)
- Testing: `PostgresHandler<TestDbPools>` (enforced separation)

**Serialization with Fallback:**

Request/response bodies attempt structured parsing, fallback to raw strings:

```rust
type RequestSerializer<T> = Arc<dyn Fn(&RequestData) -> Result<T, SerializationError>>;

// Stores either:
// - Ok(T) if serialization succeeded → body_parsed = true
// - Err(String) if failed → body_parsed = false, stores raw UTF-8
```

This ensures no data loss even when bodies can't be parsed as the expected type.

## Common Tasks

### Running Tests

```bash
# All tests
cargo test

# Only unit tests (faster)
cargo test --lib

# Specific test
cargo test test_write_operations_use_write_pool

# With output
cargo test -- --nocapture
```

### Adding New Features

1. Create feature branch: `git checkout -b feature-name`
2. Implement feature with tests
3. Run full test suite: `cargo test`
4. Run linter: `cargo clippy --all-targets --all-features -- -D warnings`
5. Format code: `cargo fmt`
6. Commit with conventional commits format
7. When ready, squash merge to `main`

### Adding New Tests

**For read/write separation:**
Use `TestDbPools` to verify correct pool usage:

```rust
#[sqlx::test]
async fn test_new_feature_uses_correct_pool(pool: PgPool) {
    crate::migrator().run(&pool).await.unwrap();
    let test_pools = TestDbPools::new(pool).await.unwrap();
    let handler = PostgresHandler::from_pool_provider(test_pools).await.unwrap();

    // Your test here - writes to .read() will fail
}
```

**For typed bodies:**
Define test types and verify serialization:

```rust
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct TestRequest { /* ... */ }

#[sqlx::test]
async fn test_typed_serialization(pool: PgPool) {
    let handler = PostgresHandler::<PgPool, TestRequest, TestResponse>::from_pool(pool)
        .await
        .unwrap();
    // Test serialization logic
}
```

### Fixing Doctest Issues

If doctests fail due to type inference:

```rust
// ❌ Wrong - P defaults to PgPool but we're passing MyBodyType
PostgresHandler::<MyBodyType, MyBodyType>::new(url).await?

// ✅ Correct - Use _ to infer P, specify TReq and TRes
PostgresHandler::<_, MyBodyType, MyBodyType>::new(url).await?
```

## Dependencies

### Core Dependencies

- `outlet` - HTTP middleware framework (trait definitions)
- `sqlx` - Async PostgreSQL driver
- `sqlx-pool-router` - Read/write pool routing (local dependency)
- `tokio` - Async runtime
- `serde` / `serde_json` - Serialization
- `uuid` - Instance IDs and correlation
- `chrono` - Timestamps
- `metrics` - Prometheus-style metrics

### Dev Dependencies

- `tokio-test` - Async test utilities
- `tracing-test` - Test logging
- `axum` / `tower` - Example web server
- `sqlparser` - SQL validation in repository tests

## AI Agent Guidelines

When working on this codebase as an AI agent:

### Before Making Changes

1. **Read existing tests** to understand patterns
2. **Check both `src/lib.rs` and `src/repository.rs`** for related code
3. **Verify pool usage** - writes use `.write()`, reads use `.read()`

### When Adding Features

1. **Add tests first** or alongside implementation
2. **Use `TestDbPools`** if touching read/write logic
3. **Maintain type safety** - don't remove generic parameters
4. **Update examples** if adding public APIs

### When Fixing Issues

1. **Write a failing test** that reproduces the issue
2. **Fix the issue** and verify test passes
3. **Check for similar patterns** elsewhere in codebase
4. **Update documentation** if behavior changes

### Before Committing

1. Run `cargo test` - all tests must pass
2. Run `cargo clippy --all-targets --all-features -- -D warnings` - zero warnings
3. Run `cargo fmt` - code must be formatted
4. Write conventional commit message
5. Include `Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>`

### Working with sqlx-pool-router

This is a **local dependency** at `../sqlx-pool-router`. Changes may be needed in both repos.

**Key types:**
- `PoolProvider` trait - Defines `.read()` and `.write()`
- `DbPools` - Production read/write separation
- `TestDbPools` - Test helper with read-only replica

**When making changes:**
1. Check if `sqlx-pool-router` needs updates
2. Test changes in both repos
3. Update `Cargo.toml` path if needed

## Troubleshooting

### Tests Fail with "role does not exist"

Update `.env` with your PostgreSQL username:
```bash
DATABASE_URL="postgresql://your_username@localhost/postgres"
```

### Tests Hang or Timeout

- PostgreSQL may not be running: `brew services start postgresql` (macOS)
- Check connection: `psql -l`

### Clippy Errors

Run clippy and fix all warnings:
```bash
cargo clippy --all-targets --all-features -- -D warnings
```

Common issues:
- Manual `div_ceil` - use `.div_ceil()` method
- Unnecessary clones - remove if not needed
- Complex type annotations - simplify or add comments

### Doctest Compilation Errors

Check type parameters in doc examples:
- Use `_` for inferred pool type
- Specify body types explicitly: `PostgresHandler::<_, ReqType, ResType>`

## Future Improvements

Ideas for future development:

- [ ] Add connection pool metrics (active connections, wait times)
- [ ] Support for response compression detection in serializers
- [ ] Batch writes for high-throughput scenarios
- [ ] Retention policies / automatic cleanup of old logs
- [ ] Query performance optimizations (indexes, materialized views)
- [ ] Support for other databases (MySQL, SQLite) via sqlx

## Resources

- **outlet framework**: https://github.com/doublewordai/outlet
- **sqlx**: https://github.com/launchbadge/sqlx
- **Conventional Commits**: https://www.conventionalcommits.org/
- **Rust async book**: https://rust-lang.github.io/async-book/
