# Changelog

## Unreleased

## [0.1.2] - 2026-07-27

### Fixed
- Cloud relay (`tanuki --cloud <device-token-id>`) no longer crashes on connect with "Cannot start a runtime from within a runtime". `CloudDuplex` writes now run on a dedicated thread instead of calling `rust_socketio`'s synchronous `.emit()` from inside the client's own tokio runtime.
- `tanuki server --cloud-host` no longer prints "cloud host active" when the cloud relay connection actually failed to start.

## [0.1.1] - 2026-07-26

### Added
- `tanuki server --cloud-host` now reports cloud host status in its ready message.

### Fixed
- Removed leftover Herdr-era plugin manifest sentinels and fixtures.
- Windows: fixed dead-code warnings and a refutable pattern match in the headless server.

## [0.1.0] - 2026-07-26

### Changed
- Initial release under the Tanuki name (renamed from Tanuki Term).
