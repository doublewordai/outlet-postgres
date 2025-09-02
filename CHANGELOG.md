# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1](https://github.com/doublewordai/outlet-postgres/compare/v0.3.0...v0.3.1) - 2025-09-02

### Other

- Update outlet and outlet-postgres versions in README
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
