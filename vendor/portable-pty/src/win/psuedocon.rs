use super::WinChild;
use crate::cmdbuilder::CommandBuilder;
use crate::win::procthreadattr::ProcThreadAttributeList;
use anyhow::{bail, ensure, Error};
use filedescriptor::{FileDescriptor, OwnedHandle};
use lazy_static::lazy_static;
use shared_library::shared_library;
use std::ffi::OsString;
use std::io::Error as IoError;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::sync::Mutex;
use std::{mem, ptr};
use winapi::shared::minwindef::DWORD;
use winapi::shared::winerror::{HRESULT, S_OK};
use winapi::um::handleapi::*;
use winapi::um::processthreadsapi::*;
use winapi::um::winbase::{
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};
use winapi::um::wincon::COORD;
use winapi::um::winnt::HANDLE;

pub type HPCON = HANDLE;

pub const PSUEDOCONSOLE_INHERIT_CURSOR: DWORD = 0x1;
pub const PSEUDOCONSOLE_RESIZE_QUIRK: DWORD = 0x2;
pub const PSEUDOCONSOLE_WIN32_INPUT_MODE: DWORD = 0x4;
#[allow(dead_code)]
pub const PSEUDOCONSOLE_PASSTHROUGH_MODE: DWORD = 0x8;

shared_library!(ConPtyFuncs,
    pub fn CreatePseudoConsole(
        size: COORD,
        hInput: HANDLE,
        hOutput: HANDLE,
        flags: DWORD,
        hpc: *mut HPCON
    ) -> HRESULT,
    pub fn ResizePseudoConsole(hpc: HPCON, size: COORD) -> HRESULT,
    pub fn ClosePseudoConsole(hpc: HPCON),
);

fn load_conpty() -> ConPtyFuncs {
    // If the kernel doesn't export these functions then their system is
    // too old and we cannot run.
    let kernel = ConPtyFuncs::open(Path::new("kernel32.dll")).expect(
        "this system does not support conpty.  Windows 10 October 2018 or newer is required",
    );

    kernel
}

lazy_static! {
    static ref CONPTY: ConPtyFuncs = load_conpty();
}

pub struct PsuedoCon {
    con: HPCON,
}

unsafe impl Send for PsuedoCon {}
unsafe impl Sync for PsuedoCon {}

impl Drop for PsuedoCon {
    fn drop(&mut self) {
        unsafe { (CONPTY.ClosePseudoConsole)(self.con) };
    }
}

/// Mirrors the undocumented `_PseudoConsole` struct that `HPCON` actually
/// points to (verified against `microsoft/terminal`'s `src/winconpty/winconpty.h`,
/// and confirmed empirically: duplicating exactly these 3 handles into another
/// process and heap-reconstructing this struct there produces an `HPCON` that
/// `ResizePseudoConsole` accepts and that the live shell honors -- see the
/// `conpty-handoff` scratch experiment referenced in the handoff design notes).
///
/// This is not a portable-pty concept; it exists only to support live
/// hand-off of an owned pseudoconsole to a successor process (e.g. during a
/// self-update), which requires reaching past the opaque `HPCON` to the raw
/// kernel handles so they can be duplicated with `DuplicateHandle`.
#[repr(C)]
struct PseudoConsoleInternal {
    h_signal: HANDLE,
    h_pty_reference: HANDLE,
    h_conpty_process: HANDLE,
}

/// The raw handles behind a live `HPCON`, extracted for cross-process
/// hand-off. Each field is a `HANDLE` value still owned by the process that
/// extracted it -- the receiving process must `DuplicateHandle` these into
/// its own handle table before use; the values are meaningless as-is in any
/// other process.
#[derive(Debug, Clone, Copy)]
pub struct PseudoConsoleHandoffHandles {
    pub signal: HANDLE,
    pub pty_reference: HANDLE,
    pub conpty_process: HANDLE,
}

impl PsuedoCon {
    /// Reads out the raw handles behind this `HPCON` without taking
    /// ownership of them or affecting this `PsuedoCon`'s normal lifecycle.
    /// The caller is responsible for duplicating them into a target process
    /// before this `PsuedoCon` is dropped (which still calls
    /// `ClosePseudoConsole` normally, as if `handoff_handles` were never
    /// called).
    pub fn handoff_handles(&self) -> PseudoConsoleHandoffHandles {
        let internal = unsafe { &*(self.con as *const PseudoConsoleInternal) };
        PseudoConsoleHandoffHandles {
            signal: internal.h_signal,
            pty_reference: internal.h_pty_reference,
            conpty_process: internal.h_conpty_process,
        }
    }

    /// Consumes this `PsuedoCon` for hand-off to a successor process,
    /// returning the same raw handles as [`Self::handoff_handles`] but
    /// suppressing this instance's `Drop` impl so it does NOT call
    /// `ClosePseudoConsole` -- only the caller's own, now-transferred-out
    /// copies of the handles are closed, exactly as validated by
    /// Experiment 2a in the handoff design notes (owner exits without ever
    /// calling `ClosePseudoConsole`). Deliberately never calls
    /// `ClosePseudoConsole` on the hand-off path at all, on any Windows
    /// version -- that sidesteps the documented pre-Windows-11-24H2
    /// blocking-wait behavior of that function entirely, rather than
    /// relying on it behaving safely when other handle-holders still
    /// exist.
    pub fn into_handoff_handles(self) -> PseudoConsoleHandoffHandles {
        let handles = self.handoff_handles();
        std::mem::forget(self);
        handles
    }

    /// Reconstructs a `PsuedoCon` in this process from handles that were
    /// already `DuplicateHandle`'d into this process's handle table by the
    /// process that called [`Self::into_handoff_handles`] (or
    /// [`Self::handoff_handles`]). The returned `PsuedoCon` behaves like any
    /// other -- its `Drop` impl calls `ClosePseudoConsole` normally, because
    /// this process is now the legitimate owner going forward.
    ///
    /// # Safety
    /// `handles` must contain valid, already-duplicated-into-this-process
    /// handle values that together describe a still-live pseudoconsole
    /// (typically obtained via a prior [`Self::into_handoff_handles`] call
    /// in another process, transported out-of-band, and duplicated in with
    /// `DuplicateHandle`).
    pub unsafe fn from_handoff_handles(handles: PseudoConsoleHandoffHandles) -> Self {
        let internal = Box::new(PseudoConsoleInternal {
            h_signal: handles.signal,
            h_pty_reference: handles.pty_reference,
            h_conpty_process: handles.conpty_process,
        });
        Self {
            con: Box::into_raw(internal) as HPCON,
        }
    }
}

impl PsuedoCon {
    pub fn new(size: COORD, input: FileDescriptor, output: FileDescriptor) -> Result<Self, Error> {
        let mut con: HPCON = INVALID_HANDLE_VALUE;
        let result = unsafe {
            (CONPTY.CreatePseudoConsole)(
                size,
                input.as_raw_handle() as _,
                output.as_raw_handle() as _,
                PSUEDOCONSOLE_INHERIT_CURSOR
                    | PSEUDOCONSOLE_RESIZE_QUIRK
                    | PSEUDOCONSOLE_WIN32_INPUT_MODE,
                &mut con,
            )
        };
        ensure!(
            result == S_OK,
            "failed to create psuedo console: HRESULT {}",
            result
        );
        Ok(Self { con })
    }

    pub fn resize(&self, size: COORD) -> Result<(), Error> {
        let result = unsafe { (CONPTY.ResizePseudoConsole)(self.con, size) };
        ensure!(
            result == S_OK,
            "failed to resize console to {}x{}: HRESULT: {}",
            size.X,
            size.Y,
            result
        );
        Ok(())
    }

    pub fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<WinChild> {
        let mut si: STARTUPINFOEXW = unsafe { mem::zeroed() };
        si.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
        // Explicitly set the stdio handles as invalid handles otherwise
        // we can end up with a weird state where the spawned process can
        // inherit the explicitly redirected output handles from its parent.
        // For example, when daemonizing wezterm-mux-server, the stdio handles
        // are redirected to a log file and the spawned process would end up
        // writing its output there instead of to the pty we just created.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;

        let mut attrs = ProcThreadAttributeList::with_capacity(1)?;
        attrs.set_pty(self.con)?;
        si.lpAttributeList = attrs.as_mut_ptr();

        let mut pi: PROCESS_INFORMATION = unsafe { mem::zeroed() };

        let (mut exe, mut cmdline) = cmd.cmdline()?;
        let cmd_os = OsString::from_wide(&cmdline);

        let cwd = cmd.current_directory();

        let res = unsafe {
            CreateProcessW(
                exe.as_mut_slice().as_mut_ptr(),
                cmdline.as_mut_slice().as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                cmd.environment_block().as_mut_slice().as_mut_ptr() as *mut _,
                cwd.as_ref()
                    .map(|c| c.as_slice().as_ptr())
                    .unwrap_or(ptr::null()),
                &mut si.StartupInfo,
                &mut pi,
            )
        };
        if res == 0 {
            let err = IoError::last_os_error();
            let msg = format!(
                "CreateProcessW `{:?}` in cwd `{:?}` failed: {}",
                cmd_os,
                cwd.as_ref().map(|c| OsString::from_wide(c)),
                err
            );
            log::error!("{}", msg);
            bail!("{}", msg);
        }

        // Make sure we close out the thread handle so we don't leak it;
        // we do this simply by making it owned
        let _main_thread = unsafe { OwnedHandle::from_raw_handle(pi.hThread as _) };
        let proc = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as _) };

        Ok(WinChild {
            proc: Mutex::new(proc),
        })
    }
}

#[cfg(test)]
mod handoff_tests {
    use super::*;
    use filedescriptor::Pipe;

    /// Same-process round-trip: extract handoff handles from a live
    /// `PsuedoCon`, reconstruct a second `PsuedoCon` from those exact
    /// values (valid here without `DuplicateHandle` because it's still the
    /// same process/handle table), and confirm the reconstructed instance
    /// can still resize the pseudoconsole. This doesn't exercise the
    /// cross-process `DuplicateHandle` step (covered by the standalone
    /// `conpty-handoff` scratch harness instead), but it does catch a
    /// regression in the `PseudoConsoleInternal` field layout/order, which
    /// is the part most likely to silently break on a future Windows
    /// update.
    #[test]
    fn handoff_handles_round_trip_reconstructs_usable_hpcon() {
        let stdin = Pipe::new().expect("stdin pipe");
        let stdout = Pipe::new().expect("stdout pipe");
        let con = PsuedoCon::new(COORD { X: 80, Y: 25 }, stdin.read, stdout.write)
            .expect("CreatePseudoConsole");

        let handles = con.handoff_handles();
        assert!(!handles.signal.is_null());
        assert!(!handles.pty_reference.is_null());
        assert!(!handles.conpty_process.is_null());

        let reconstructed = unsafe { PsuedoCon::from_handoff_handles(handles) };
        reconstructed
            .resize(COORD { X: 100, Y: 30 })
            .expect("resize on reconstructed HPCON should succeed");

        // Both `con` and `reconstructed` now think they own the same
        // underlying handles. Forget one to avoid a double
        // ClosePseudoConsole in this same-process test (in the real
        // cross-process flow, only the successor ever holds a live
        // PsuedoCon at this point -- the owner already forgot its own via
        // into_handoff_handles).
        std::mem::forget(con);
        drop(reconstructed);
    }
}
