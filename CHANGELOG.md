# Changelog

All notable changes to this project will be documented in this file.

## 0.1.8 - 2026-03-17
### Changed
- improve performance by avoiding some copies and allocations

## 0.1.7 - 2025-03-07
### Changed
- Standard interface: no delay when checking for send space.
- Stream interface: improve send performance.

## 0.1.6 - 2025-03-05
### Fixed
- Standard interface: fix race condition when connecting.

## 0.1.5 - 2025-03-05
### Changed
- Standard interface: make flush a no-op.

## 0.1.4 - 2025-01-25
### Fixed
- WebSocketStream: do not wait for ready on flush.
- Catch exception on WebSocketStream creation.

## 0.1.3 - 2025-01-21
- Initial release.
