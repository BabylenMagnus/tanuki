# portable-pty local patches

This file tracks intentional local changes applied on top of the vendored
`portable-pty` source. Remove a patch only when the upstream crate contains an
equivalent fix or exposes an option that lets Herdr keep the same behavior.

## 0001 force system ConPTY

status: active

patch: `vendor/patches/portable-pty/0001-force-system-conpty.patch`

herdr issue: https://github.com/ogulcancelik/herdr/issues/761

upstream discussion: none found

upstream pr: none

vendored base: `portable-pty 0.9.0`

local files:

- `vendor/portable-pty/src/win/psuedocon.rs`

reason: `portable-pty` intentionally probes a bare `conpty.dll` after verifying
that `kernel32.dll` exports the ConPTY API. That is useful for WezTerm's bundled
`OpenConsole.exe` and `conpty.dll` pair, but Herdr does not ship that pair and
must not load another application's `conpty.dll` from `PATH`.

remove when: upstream `portable-pty` no longer loads bare `conpty.dll` from the
DLL search path, upstream exposes a way for consumers to force system ConPTY, or
Herdr replaces the Windows PTY backend.

verification:

```sh
python3 -m unittest scripts.test_vendor_portable_pty
```

On Windows, also verify that pane creation succeeds when `PATH` contains a
directory with `conpty.dll`.

## 0002 expose Windows raw command tails

status: active

patch: `vendor/patches/portable-pty/0002-windows-raw-command-tail.patch`

herdr issue: https://github.com/ogulcancelik/herdr/issues/1041

upstream discussion: none

upstream pr: none

vendored base: `portable-pty 0.9.0`

local files:

- `vendor/portable-pty/src/cmdbuilder.rs`

reason: Herdr needs to launch `cmd.exe /d /c` with the user-authored command
tail parsed as shell text. `portable-pty` represents commands as argv and
ArgvQuote escapes embedded quotes, which changes how `cmd.exe` parses the raw
command string.

remove when: upstream `portable-pty` exposes Windows raw command-line tail
support or Herdr replaces this launch path.

verification:

```sh
python3 -m unittest scripts.test_vendor_portable_pty
```

On Windows, also run `cargo test raw_arg_appends_unescaped_windows_command_tail`.

## 0003 expose ConPTY handles for cross-process hand-off

status: active

patch: `vendor/patches/portable-pty/0003-expose-conpty-handoff-handles.patch`

tanuki issue: TSK-17 (self-update reliability) design work -- live handoff of
a running server process's PTY sessions on Windows, mirroring the existing
Unix `server/handoff.rs` (SCM_RIGHTS) mechanism.

upstream discussion: none found

upstream pr: none

vendored base: `portable-pty 0.9.0`

local files:

- `vendor/portable-pty/src/win/psuedocon.rs`
- `vendor/portable-pty/src/win/conpty.rs`

reason: `portable-pty`'s Windows backend (ConPTY) keeps `HPCON` and the pty's
pipe handles fully opaque -- `MasterPty`'s trait object exposes only
`Read`/`Write`/`resize`, never a raw handle a successor process could
`DuplicateHandle` in to keep the shell alive across a self-update that
replaces the owning `tanuki server` process. This patch adds
`PsuedoCon::{handoff_handles,into_handoff_handles,from_handoff_handles}` and
`ConPtyMasterPty::into_handoff` / `master_from_handoff`, reaching past the
opaque `HPCON` to the undocumented `_PseudoConsole{hSignal,hPtyReference,
hConPtyProcess}` struct it actually points to (layout confirmed against
`microsoft/terminal`'s `src/winconpty/winconpty.h`).

Verified empirically with a standalone `windows-sys` scratch harness on
build 26200 (Windows 11 24H2+): duplicating those 3 handles plus the
input/output pipe handles into a second process and reconstructing the
struct there produces a live `HPCON` that `ResizePseudoConsole` accepts and
the running shell honors, even after the original owner process exits --
confirmed via a real resize round-trip (`mode con`: 80x25 -> 90x28) and
independently via the XTWINOPS `ESC[8;28;90t` sequence conhost itself
emitted after the resize. Held across every owner-teardown order tested:
exiting without ever calling `ClosePseudoConsole`, calling it explicitly,
and tearing down immediately without waiting for the successor's ack.

The hand-off path deliberately never calls `ClosePseudoConsole` on any
Windows version, sidestepping the documented pre-Windows-11-24H2
blocking-wait behavior of that function entirely rather than relying on
the empirically-observed post-24H2 safety (that range of builds is
untested -- see backlog below).

remove when: tanuki replaces the Windows PTY backend, or upstream
`portable-pty` grows first-class hand-off support.

backlog (not blocking this patch, blocking only the "supported feature"
announcement):

- verify on a pre-24H2 Windows 10 / Windows 11 build (no VM available at
  patch time)
- repeat the round-trip with a real long-running agent CLI producing active
  ANSI output at the moment of cutover, to measure any output-byte gap
- verify a second hand-off generation (successor -> successor2)

verification:

```sh
cargo build --bin tanuki --target x86_64-pc-windows-msvc
cargo clippy --bin tanuki --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo fmt --check
```

On Windows, this compiles as part of the normal `tanuki` build (the new
methods are additive and unused until tanuki's own Windows handoff code
calls them).
