# Changelog

## Unreleased

### Added
- Log panic information using tracing for better debugging

## [0.1.73] - 2026-08-10

### Fixed
- Parse ICE candidate JSON to avoid double-encoding errors

## [0.1.72] - 2026-08-10

### Added
- Add spawn_socket_emit for safe socket emissions from handlers

## [0.1.71] - 2026-08-05

### Added
- Implement replay buffer for viewer sessions in CloudDuplex

### Changed
- Bump version to 0.1.70 and update changelog
- Populate Unreleased changelog from commits

## [0.1.70] - 2026-08-05

### Added
- Implement replay buffer for viewer sessions in CloudDuplex

## [0.1.69] - 2026-08-04

### Added
- Add support for cloud host registration on server launch

## [0.1.68] - 2026-08-03

### Added
- Update global menu action to 'update & restart' and implement logic

## [0.1.67] - 2026-08-03

### Fixed
- Update Russian translations and improve settings popup dimensions
- Preserve launch_argv during terminal state reset

### Changed
- Bump version to 0.1.66 and update changelog
- Populate Unreleased changelog from commits

## [0.1.66] - 2026-08-03

### Fixed
- Update Russian translations and improve settings popup dimensions
- Preserve launch_argv during terminal state reset

## [0.1.65] - 2026-08-03

### Fixed
- Increase popup width from 76 to 104 pixels

## [0.1.64] - 2026-08-03

### Changed
- Improve language handling and sidebar layout calculations

## [0.1.63] - 2026-08-03

### Added
- Add support for respawning server daemon after updates

## [0.1.62] - 2026-08-02

### Added
- Add pre-reform Russian language support in config

## [0.1.61] - 2026-08-02

### Added
- Update dependencies and add localization support for English

### Changed
- Bump version to 0.1.60 and update changelog with new features
- Populate Unreleased changelog from commits
- Bump version to 0.1.59 and update changelog
- Populate Unreleased changelog from commits

## [0.1.60] - 2026-08-02

### Added
- Update dependencies and add localization support for English

### Changed
- Bump version to 0.1.59 and update changelog
- Populate Unreleased changelog from commits

## [0.1.59] - 2026-08-02

### Added
- Update dependencies and add localization support for English

## [0.1.58] - 2026-08-02

### Added
- Add FORK.md to document fork provenance and upstream sync strategy

### Fixed
- Use or() instead of or_else() for clippy unnecessary_lazy_evaluations

### Changed
- Bump version to 0.1.57 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.56 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.55 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.54 and update changelog
- Populate Unreleased changelog from commits

## [0.1.57] - 2026-08-02

### Added
- Add FORK.md to document fork provenance and upstream sync strategy

### Fixed
- Use or() instead of or_else() for clippy unnecessary_lazy_evaluations

### Changed
- Bump version to 0.1.56 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.55 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.54 and update changelog
- Populate Unreleased changelog from commits

## [0.1.56] - 2026-08-02

### Added
- Add FORK.md to document fork provenance and upstream sync strategy

### Fixed
- Use or() instead of or_else() for clippy unnecessary_lazy_evaluations

### Changed
- Bump version to 0.1.55 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.54 and update changelog
- Populate Unreleased changelog from commits

## [0.1.55] - 2026-08-02

### Added
- Add FORK.md to document fork provenance and upstream sync strategy

### Changed
- Bump version to 0.1.54 and update changelog
- Populate Unreleased changelog from commits

## [0.1.54] - 2026-08-02

### Added
- Add FORK.md to document fork provenance and upstream sync strategy

## [0.1.53] - 2026-08-02

### Added
- Add equalize splits action and update version to 0.1.49
- Implement semantic frame writing and patch handling in client

### Changed
- Bump version to 0.1.52 and update changelog
- Populate Unreleased changelog from commits
- Update version to 0.1.51 and add changelog entry
- Populate Unreleased changelog from commits
- Bump version to 0.1.50 and update changelog
- Populate Unreleased changelog from commits
- Populate Unreleased changelog from commits
- Bump version to 0.1.48 and update changelog
- Populate Unreleased changelog from commits

## [0.1.52] - 2026-08-02

### Added
- Add equalize splits action and update version to 0.1.49
- Implement semantic frame writing and patch handling in client

### Changed
- Update version to 0.1.51 and add changelog entry
- Populate Unreleased changelog from commits
- Bump version to 0.1.50 and update changelog
- Populate Unreleased changelog from commits
- Populate Unreleased changelog from commits
- Bump version to 0.1.48 and update changelog
- Populate Unreleased changelog from commits

## [0.1.51] - 2026-08-02

### Added
- Add equalize splits action and update version to 0.1.49
- Implement semantic frame writing and patch handling in client

### Changed
- Bump version to 0.1.50 and update changelog
- Populate Unreleased changelog from commits
- Populate Unreleased changelog from commits
- Bump version to 0.1.48 and update changelog
- Populate Unreleased changelog from commits

## [0.1.50] - 2026-08-02

### Added
- Add equalize splits action and update version to 0.1.49
- Implement semantic frame writing and patch handling in client

### Changed
- Populate Unreleased changelog from commits
- Bump version to 0.1.48 and update changelog
- Populate Unreleased changelog from commits

## [0.1.49] - 2026-08-01

### Added
- Implement semantic frame writing and patch handling in client

### Changed
- Bump version to 0.1.48 and update changelog
- Populate Unreleased changelog from commits

## [0.1.48] - 2026-08-01

### Added
- Implement semantic frame writing and patch handling in client

## [0.1.47] - 2026-07-31

### Fixed
- Stop forcing full-screen client clears on every keystroke: terminal input no longer requests full re-render (PTY retained path handles content), and same-size full rebuilds set is_full=false so the client diffs instead of CSI 2J
- Do not reset the semantic last-frame baseline on every input event (was forcing non-skippable full paints)

## [0.1.46] - 2026-07-31

### Fixed
- Keep GhosttyPaneTerminal's per-row render cache coherent with `collect_dirty_patch`: successful retained patches now write updated cells into the cache, and fallbacks invalidate it, so full renders no longer serve stale rows after the retained path consumed shared terminal dirty flags (staircase / freeze regression from the v0.1.42–v0.1.45 cache)

### Changed
- Fall back to an existing prebuilt libghostty-vt artifact when the local Zig rebuild fails (and allow `LIBGHOSTTY_VT_USE_PREBUILT=1` to skip Zig entirely)

## [0.1.45] - 2026-07-31

### Added
- Add per-row cache for rendered cells in GhosttyPaneTerminal

### Changed
- Bump version to 0.1.44 and update changelog
- Populate Unreleased changelog from commits

## [0.1.44] - 2026-07-31

### Added
- Add per-row cache for rendered cells in GhosttyPaneTerminal

## [0.1.43] - 2026-07-31

### Changed
- Remove RenderedRowCache from GhosttyPaneCore struct

## [0.1.42] - 2026-07-31

### Added
- Add per-row cache for rendered cells in GhosttyPaneTerminal

## [0.1.41] - 2026-07-31

### Added
- Add performance profiling for terminal resize operations

## [0.1.40] - 2026-07-31

### Fixed
- Update clipboard and input source handling to reflect local effects

### Changed
- Bump version to 0.1.39 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.38 and update changelog
- Populate Unreleased changelog from commits

## [0.1.39] - 2026-07-31

### Fixed
- Update clipboard and input source handling to reflect local effects

### Changed
- Bump version to 0.1.38 and update changelog
- Populate Unreleased changelog from commits

## [0.1.38] - 2026-07-31

### Fixed
- Update clipboard and input source handling to reflect local effects

## [0.1.37] - 2026-07-31

### Added
- Add performance profiling for layout and draw phases

## [0.1.36] - 2026-07-31

### Added
- Track terminal cwd changes to optimize rendering performance

## [0.1.35] - 2026-07-30

### Added
- Add diagnostic logging for full redraw events in client loop

## [0.1.34] - 2026-07-30

### Added
- Track changes from git status refresh for optimized rendering

## [0.1.33] - 2026-07-30

### Added
- Add debounce delay for initial render to reduce tearing

## [0.1.32] - 2026-07-30

### Added
- Add synchronized output support for frame rendering

## [0.1.31] - 2026-07-30

### Added
- Add diagnostic logging for full render causes in headless server

### Changed
- Bump version to 0.1.30 and update changelog
- Populate Unreleased changelog from commits

## [0.1.30] - 2026-07-30

### Added
- Add diagnostic logging for full render causes in headless server

## [0.1.29] - 2026-07-30

### Added
- Update version to 0.1.27 and enhance changelog notes

### Changed
- Bump version to 0.1.28 and update changelog entries
- Populate Unreleased changelog from commits

## [0.1.28] - 2026-07-30

### Added
- Update version to 0.1.27 and enhance changelog notes

## [0.1.27] - 2026-07-30

### Added
- Read changelog notes from CHANGELOG.md in release manifest generation

## [0.1.26] - 2026-07-30

### Added
- Update changelog and latest.json for version 0.1.24 release

### Changed
- Bump version to 0.1.25 and update changelog
- Populate Unreleased changelog from commits

## [0.1.25] - 2026-07-30

### Added
- Update changelog and latest.json for version 0.1.24 release

## [0.1.24] - 2026-07-30

### Added
- Add protocol version extraction to release manifest generation

## [0.1.23] - 2026-07-30

### Added
- Release version 0.1.21 with updated dependencies and changelog
- Introduce MIN_FULL_RENDER_INTERVAL for throttling full renders
- Add release 0.1.14 and update latest version

### Changed
- Bump version to 0.1.22 and update changelog
- Populate Unreleased changelog from commits
- Populate Unreleased changelog from commits
- Bump version to 0.1.20 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.19 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.18 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.17 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.16 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.15 and update changelog
- Populate Unreleased changelog from commits

## [0.1.22] - 2026-07-30

### Added
- Release version 0.1.21 with updated dependencies and changelog
- Introduce MIN_FULL_RENDER_INTERVAL for throttling full renders
- Add release 0.1.14 and update latest version

### Changed
- Populate Unreleased changelog from commits
- Bump version to 0.1.20 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.19 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.18 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.17 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.16 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.15 and update changelog
- Populate Unreleased changelog from commits

## [0.1.21] - 2026-07-30

### Added
- Introduce MIN_FULL_RENDER_INTERVAL for throttling full renders
- Add release 0.1.14 and update latest version

### Changed
- Bump version to 0.1.20 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.19 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.18 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.17 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.16 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.15 and update changelog
- Populate Unreleased changelog from commits

## [0.1.20] - 2026-07-30

### Added
- Introduce MIN_FULL_RENDER_INTERVAL for throttling full renders
- Add release 0.1.14 and update latest version

### Changed
- Bump version to 0.1.19 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.18 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.17 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.16 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.15 and update changelog
- Populate Unreleased changelog from commits

## [0.1.19] - 2026-07-30

### Added
- Introduce MIN_FULL_RENDER_INTERVAL for throttling full renders
- Add release 0.1.14 and update latest version

### Changed
- Bump version to 0.1.18 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.17 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.16 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.15 and update changelog
- Populate Unreleased changelog from commits

## [0.1.18] - 2026-07-30

### Added
- Introduce MIN_FULL_RENDER_INTERVAL for throttling full renders
- Add release 0.1.14 and update latest version

### Changed
- Bump version to 0.1.17 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.16 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.15 and update changelog
- Populate Unreleased changelog from commits

## [0.1.17] - 2026-07-30

### Added
- Introduce MIN_FULL_RENDER_INTERVAL for throttling full renders
- Add release 0.1.14 and update latest version

### Changed
- Bump version to 0.1.16 and update changelog
- Populate Unreleased changelog from commits
- Bump version to 0.1.15 and update changelog
- Populate Unreleased changelog from commits

## [0.1.16] - 2026-07-30

### Added
- Introduce MIN_FULL_RENDER_INTERVAL for throttling full renders
- Add release 0.1.14 and update latest version

### Changed
- Bump version to 0.1.15 and update changelog
- Populate Unreleased changelog from commits

## [0.1.15] - 2026-07-30

### Added
- Introduce MIN_FULL_RENDER_INTERVAL for throttling full renders
- Add release 0.1.14 and update latest version

## [0.1.14] - 2026-07-30

### Changed
- Vendored `portable-pty` (Windows ConPTY backend) now exposes the raw handles behind a live pseudoconsole (`PsuedoCon::{handoff_handles,into_handoff_handles,from_handoff_handles}`, `ConPtyMasterPty::into_handoff`/`master_from_handoff`) for cross-process hand-off, laying the groundwork for a Windows equivalent of the existing Unix `server/handoff.rs` self-update path (live session hand-off without killing running shells). This release only adds the capability to the vendored crate; nothing in `tanuki server`/`tanuki update` calls it yet, so there is no user-visible behavior change. Verified empirically against real Windows ConPTY internals with a standalone scratch harness before landing (see `vendor/portable-pty.patches.md`, patch 0003).

## [0.1.13] - 2026-07-30

### Fixed
- Panes launched with custom CLI flags (e.g. `claude --dangerously-skip-permissions`) no longer lose them across a native session restore. The restore path saved the pane's original launch argv (`saved_launch_argv`) but never read it back on the native-agent-resume branch, always using the hardcoded per-agent resume template (e.g. `["claude", "--resume", "<id>"]`) verbatim instead. The template now keeps any extra flags from the original launch that it doesn't already set itself. Affects all agents built on the shared native-resume mechanism (claude, codex, copilot, devin, droid, omp, qodercli, cursor, hermes, pi, kilo, mastracode).

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
