use crate::cmdbuilder::CommandBuilder;
use crate::win::psuedocon::{PseudoConsoleHandoffHandles, PsuedoCon};
use crate::{Child, MasterPty, PtyPair, PtySize, PtySystem, SlavePty};
use anyhow::Error;
use filedescriptor::{FileDescriptor, Pipe};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::sync::{Arc, Mutex};
use winapi::um::wincon::COORD;

#[derive(Default)]
pub struct ConPtySystem {}

impl PtySystem for ConPtySystem {
    fn openpty(&self, size: PtySize) -> anyhow::Result<PtyPair> {
        let stdin = Pipe::new()?;
        let stdout = Pipe::new()?;

        let con = PsuedoCon::new(
            COORD {
                X: size.cols as i16,
                Y: size.rows as i16,
            },
            stdin.read,
            stdout.write,
        )?;

        let master = ConPtyMasterPty {
            inner: Arc::new(Mutex::new(Inner {
                con,
                readable: stdout.read,
                writable: Some(stdin.write),
                size,
            })),
        };

        let slave = ConPtySlavePty {
            inner: master.inner.clone(),
        };

        Ok(PtyPair {
            master: Box::new(master),
            slave: Box::new(slave),
        })
    }
}

struct Inner {
    con: PsuedoCon,
    readable: FileDescriptor,
    writable: Option<FileDescriptor>,
    size: PtySize,
}

impl Inner {
    pub fn resize(
        &mut self,
        num_rows: u16,
        num_cols: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<(), Error> {
        self.con.resize(COORD {
            X: num_cols as i16,
            Y: num_rows as i16,
        })?;
        self.size = PtySize {
            rows: num_rows,
            cols: num_cols,
            pixel_width,
            pixel_height,
        };
        Ok(())
    }
}

#[derive(Clone)]
pub struct ConPtyMasterPty {
    inner: Arc<Mutex<Inner>>,
}

pub struct ConPtySlavePty {
    inner: Arc<Mutex<Inner>>,
}

/// The raw state behind a `ConPtyMasterPty`, extracted for cross-process
/// hand-off (e.g. a self-update replacing the owning process without
/// killing the shell running under it). Every handle field is still owned
/// by the process that produced this value -- the receiving process must
/// `DuplicateHandle` each one into its own handle table (and, for
/// `readable`/`writable`, reconstruct a `FileDescriptor` via
/// `FromRawHandle`) before use.
pub struct PtyHandoffState {
    pub pty: PseudoConsoleHandoffHandles,
    pub readable: RawHandle,
    pub writable: Option<RawHandle>,
    pub size: PtySize,
}

impl ConPtyMasterPty {
    /// Consumes this master for hand-off to a successor process. Requires
    /// this to be the sole remaining reference to the underlying pty state
    /// (true once the paired `SlavePty` returned alongside this master by
    /// `ConPtySystem::openpty` has already been dropped, which is the
    /// normal lifecycle after `spawn_command` has been called once).
    ///
    /// Like [`PsuedoCon::into_handoff_handles`], this deliberately does not
    /// close anything on the pseudoconsole/pipe path -- ownership of every
    /// handle is transferred out via [`PtyHandoffState`] and this value is
    /// forgotten, not dropped. The caller is responsible for duplicating
    /// the returned handles into the successor process and reconstructing
    /// state there (e.g. via `PsuedoCon::from_handoff_handles` and
    /// `FileDescriptor::from_raw_handle`) before this process exits.
    pub fn into_handoff(self) -> anyhow::Result<PtyHandoffState> {
        let inner = Arc::try_unwrap(self.inner)
            .map_err(|_| {
                anyhow::anyhow!(
                    "cannot hand off pty: other MasterPty/SlavePty references still exist"
                )
            })?
            .into_inner()
            .map_err(|_| anyhow::anyhow!("cannot hand off pty: inner mutex was poisoned"))?;

        let pty = inner.con.into_handoff_handles();
        let readable = inner.readable.as_raw_handle();
        let writable = inner.writable.as_ref().map(|w| w.as_raw_handle());
        let size = inner.size.clone();

        // inner.con was already handed off above (its Drop is now a no-op
        // in effect, since into_handoff_handles forgot it). readable/writable
        // still have live Drop impls that would CloseHandle the very handles
        // we just returned by value -- forget them too, mirroring the
        // pseudoconsole handoff above.
        std::mem::forget(inner.readable);
        std::mem::forget(inner.writable);

        Ok(PtyHandoffState {
            pty,
            readable,
            writable,
            size,
        })
    }
}

/// Reconstructs a `ConPtyMasterPty` (with no paired `SlavePty` -- a
/// hand-off successor only ever needs read/write/resize access, never
/// another `spawn_command`) from a [`PtyHandoffState`] whose handles have
/// already been duplicated into this process's handle table.
///
/// # Safety
/// `state`'s handles must be valid, already-duplicated-into-this-process
/// values that together describe a still-live pseudoconsole and its pipes.
pub unsafe fn master_from_handoff(state: PtyHandoffState) -> ConPtyMasterPty {
    let con = PsuedoCon::from_handoff_handles(state.pty);
    let readable = FileDescriptor::from_raw_handle(state.readable);
    let writable = state.writable.map(|h| FileDescriptor::from_raw_handle(h));

    ConPtyMasterPty {
        inner: Arc::new(Mutex::new(Inner {
            con,
            readable,
            writable,
            size: state.size,
        })),
    }
}

impl MasterPty for ConPtyMasterPty {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.resize(size.rows, size.cols, size.pixel_width, size.pixel_height)
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.size.clone())
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
        Ok(Box::new(self.inner.lock().unwrap().readable.try_clone()?))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
        Ok(Box::new(
            self.inner
                .lock()
                .unwrap()
                .writable
                .take()
                .ok_or_else(|| anyhow::anyhow!("writer already taken"))?,
        ))
    }
}

impl SlavePty for ConPtySlavePty {
    fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<Box<dyn Child + Send + Sync>> {
        let inner = self.inner.lock().unwrap();
        let child = inner.con.spawn_command(cmd)?;
        Ok(Box::new(child))
    }
}
