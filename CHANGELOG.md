# Changelog

## Unreleased

## [0.1.12] - 2026-07-30

### Changed
- Settings → Keybinds now groups shortcuts into sections (global, navigation, workspaces / tabs, panes) instead of one flat 42-item list, matching the grouping the old standalone read-only keybind-help overlay used before it was unified into this tab. Also added the navigate-mode focus/move bindings (previously not editable here at all), and read-only reference rows for the indexed `1..9` shortcuts (switch workspace/tab, focus agent) and any configured custom commands, so every shortcut that used to be visible in the old overlay is visible again — the indexed and custom-command entries keep their own binding models and aren't edited from this list.

## [0.1.11] - 2026-07-30

### Fixed
- Fixed the remaining cause of the "staircase" redraw artifact after 0.1.10: the headless server's scheduled-task handler forced a full clear-and-repaint (`needs_full_render = true`) on *every* kind of background state change, including the spinner animation tick that advances an agent's "thinking" indicator every 128ms (~8x/sec). With multiple panes/agents animating at once, this queued up several full-screen redraws per second that competed with each other and with PTY output for the terminal's synchronized-output budget, producing scattered partial repaints — worse the more panes/agents were active, matching what users reported. The spinner tick now only requests a normal diff-based render; other scheduled-task changes (toasts, notifications, metadata expiry, etc.) keep forcing a full redraw as before.

## [0.1.10] - 2026-07-30

### Fixed
- Fixed the root cause of the "staircase" redraw artifact (partial, top-to-bottom scattered repaints instead of an atomic full-screen paint), which could still appear when switching workspaces/tabs or after any large content change, especially with many panes/agents active. The server-to-client wire protocol (`FrameData`) never actually carried a "this is a full redraw" signal — the client's blit encoder always diffed incoming frames against its last locally cached frame regardless of server intent, so the earlier workspace/tab-switch fix (0.1.8) could never fully reach the client. Added a `FrameData::is_full` field, set it on the server for full-render frames, and made the client (and the server's own terminal-ANSI encoding path) honor it instead of hardcoding a diff-only blit.

## [0.1.9] - 2026-07-30

### Fixed
- `tanuki update` no longer fails with "The file or directory is not a reparse point" (os error 4390) when `current` or the visible bin dir is a plain directory rather than a junction (e.g. left over from before junction-based installs). The `junction` crate's `exists()` check errors instead of returning `false` for a plain, non-reparse-point directory that fully resolves; that specific error is now treated as "not a junction" so the existing plain-directory handling takes over.

## [0.1.8] - 2026-07-30

### Fixed
- Workspace and tab switches now force a full redraw regardless of which code path triggered them (keybinding, navigator, remote API, worktree action), fixing a scattered "staircase" redraw that could appear when switching workspaces or tabs.

## [0.1.7] - 2026-07-30

### Changed
- Update version to 0.1.6 and modify asset links in latest.json

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
