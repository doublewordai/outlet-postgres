# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.5](https://github.com/doublewordai/outlet-postgres/compare/v0.4.4...v0.4.5) - 2026-01-23

### Added

- add comprehensive test coverage for read/write pool separation

### Other

- Merge pull request #36 from doublewordai/renovate/tower-0.x-lockfile
- Merge pull request #37 from doublewordai/renovate/chrono-0.x-lockfile
- Merge pull request #38 from doublewordai/renovate/thiserror-2.x-lockfile
- add comprehensive CLAUDE.md for AI agent guidance
- *(deps)* update rust crate tokio-test to v0.4.5 ([#32](https://github.com/doublewordai/outlet-postgres/pull/32))
- *(deps)* update rust crate axum to v0.8.8 ([#31](https://github.com/doublewordai/outlet-postgres/pull/31))
- *(deps)* update rust crate tokio to v1.49.0 ([#26](https://github.com/doublewordai/outlet-postgres/pull/26))
- release v0.4.4 ([#33](https://github.com/doublewordai/outlet-postgres/pull/33))
- cargo fmt

## [0.4.4](https://github.com/doublewordai/outlet-postgres/compare/v0.4.3...v0.4.4) - 2026-01-07

### Added

- add write duration metrics

### Other

- *(deps)* update rust crate sqlparser to 0.60 ([#25](https://github.com/doublewordai/outlet-postgres/pull/25))
- *(deps)* update rust crate http to v1.4.0 ([#24](https://github.com/doublewordai/outlet-postgres/pull/24))
- *(deps)* update rust crate serde_json to v1.0.149 ([#21](https://github.com/doublewordai/outlet-postgres/pull/21))
- *(deps)* update actions/checkout action to v6 ([#27](https://github.com/doublewordai/outlet-postgres/pull/27))
- *(deps)* update rust crate bytes to v1.11.0 ([#23](https://github.com/doublewordai/outlet-postgres/pull/23))
- *(deps)* update rust crate thiserror to v2.0.17 ([#22](https://github.com/doublewordai/outlet-postgres/pull/22))
- *(deps)* update tokio-tracing monorepo ([#29](https://github.com/doublewordai/outlet-postgres/pull/29))
- *(deps)* update rust crate uuid to v1.19.0 ([#30](https://github.com/doublewordai/outlet-postgres/pull/30))
- *(deps)* update rust crate chrono to v0.4.42 ([#19](https://github.com/doublewordai/outlet-postgres/pull/19))
- *(deps)* update mozilla-actions/sccache-action action to v0.0.9 ([#15](https://github.com/doublewordai/outlet-postgres/pull/15))
- *(deps)* update rust crate axum to v0.8.7 ([#17](https://github.com/doublewordai/outlet-postgres/pull/17))
- Add renovate.json ([#14](https://github.com/doublewordai/outlet-postgres/pull/14))

## [0.4.3](https://github.com/doublewordai/outlet-postgres/compare/v0.4.2...v0.4.3) - 2025-11-11

### Fixed

- quieter logs and log lag

### Other

- update readme
- Merge branch 'main' of https://github.com/doublewordai/outlet-postgres

## [0.4.2](https://github.com/doublewordai/outlet-postgres/compare/v0.4.1...v0.4.2) - 2025-11-07

### Added

- breaking - remove path filter functionality here, and move it to outlet

## [0.4.1](https://github.com/doublewordai/outlet-postgres/compare/v0.4.0...v0.4.1) - 2025-09-30

### Fixed

- skip responses w/out a database round trip ([#9](https://github.com/doublewordai/outlet-postgres/pull/9))

## [0.4.0](https://github.com/doublewordai/outlet-postgres/compare/v0.3.1...v0.4.0) - 2025-09-25

### Added

- split off time-to-first-byte from duration

### Fixed

- clippy

### Other

- Merge pull request #7 from doublewordai/time-to-first-byte

## [0.3.1](https://github.com/doublewordai/outlet-postgres/compare/v0.3.0...v0.3.1) - 2025-09-02

### Other

- Merge branch 'main' of https://github.com/doublewordai/outlet-postgres
- Update API to match outlet v0.3.0
- Update outlet dependency to v0.3.0

## [0.3.0](https://github.com/doublewordai/outlet-postgres/compare/v0.2.0...v0.3.0) - 2025-09-01

### Fixed

- path filtering didnt take into account hosts
- add instance id so that ids are unique across restarts

## [0.2.0](https://github.com/doublewordai/outlet-postgres/compare/v0.1.2...v0.2.0) - 2025-08-29

### Added

- path filtering for logged responses
- add custom serializers, move `migrator` into standalone function

## [0.1.2](https://github.com/doublewordai/outlet-postgres/compare/v0.1.1...v0.1.2) - 2025-08-28

### Added

- enhance README with performance highlights

## [0.1.1](https://github.com/doublewordai/outlet-postgres/compare/v0.1.0...v0.1.1) - 2025-08-28

### Added

- (breaking) separate migrations from initialization

### Other

- depot
- linting
- use binstall for faster sqlx install
- fix test workflows to setup database
- extract query building logic and add comprehensive tests
- add license
