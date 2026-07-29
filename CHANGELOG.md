# Changelog

## Unreleased

## [0.1.6] - 2026-07-29

### Added
- Add script to generate release manifest from CHANGELOG.md

## [0.1.5] - 2026-07-29

### Fixed
- `tanuki update` on Windows no longer shells out to `powershell -ExecutionPolicy Bypass -Command "irm ... | iex"` to reinstall itself. That exact command line was being blocked by Windows Defender's real-time protection as a malware download-cradle pattern, making `tanuki update` fail with "Access is denied" for every Windows user. The update now downloads and installs natively in Rust (same versioned-release-dir + junction + PATH layout as `install.ps1`), never spawning PowerShell.

## [0.1.3] - 2026-07-27

### Fixed
- Cloud relay no longer hangs forever when a viewer attaches to a host registration that has gone stale (host process died without a clean disconnect). The backend now TTLs the host registration and the host refreshes it with a heartbeat; if a host_hello is rejected because a prior registration hasn't expired yet, the host retries instead of giving up silently.
- The cloud transport now honors read timeouts. Previously `Transport::Cloud`'s recv-timeout was a no-op, so any handshake that didn't complete (e.g. because of the stale-host issue above) hung indefinitely instead of failing with a clear error.

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
