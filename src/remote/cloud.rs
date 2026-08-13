//! Cloud relay transport: relays the tanuki binary wire protocol
//! through the existing Tanuki backend over Socket.IO, as an alternative to
//! the local Unix-socket / Windows-named-pipe transport (`crate::ipc`).
//!
//! This module is platform-independent (unlike `crate::remote::unix`) and
//! must build on Windows as well as Unix.
//!
//! The event contract implemented here mirrors
//! `tanuki_api/app/socket_handlers/term_relay.py` (the backend source of
//! truth) as closely as the client-side `rust_socketio` crate allows. See
//! the `ARCHITECTURE NOTE` below for one deliberate, documented deviation.
//!
//! ## ARCHITECTURE NOTE: base64-in-JSON instead of native binary attachments
//!
//! `term_relay.py` specifies that `term:frame` (host -> viewer) and
//! `term:input` (server -> host) carry `{"viewer_sid": "...", "bytes":
//! <raw binary>}`, where `bytes` is meant to be socket.io's native binary
//! attachment mechanism (a JSON payload with a nested
//! `{"_placeholder":true,"num":N}` marker, resolved against a
//! separately-transmitted binary frame).
//!
//! `rust_socketio` 0.6.0's public API cannot correctly round-trip that
//! shape. Concretely (verified by reading the vendored crate source under
//! `~/.cargo/registry/src/.../rust_socketio-0.6.0/src`):
//!
//! - Sending: `Client::emit`/`RawClient::emit` accept a single [`Payload`],
//!   which is either `Payload::Binary(Bytes)` (produces a bare
//!   `[event, {"_placeholder":true,"num":0}]` packet with no room for
//!   sibling fields like `viewer_sid`) or `Payload::Text(Vec<Value>)`
//!   (plain JSON, no binary attachment support at all). There is no public
//!   way to construct a packet with a placeholder nested inside a larger
//!   JSON object (`Socket`/`packet::Packet` are crate-private in
//!   `rust_socketio`).
//! - Receiving: `packet::Packet::try_from` reconstructs an incoming binary
//!   event by doing a literal substring replace of
//!   `{"_placeholder":true,"num":0}` with `""` inside the raw JSON text,
//!   then `client::raw_client::RawClient::handle_binary_event` derives the
//!   event name by stripping all `"` characters from *whatever text is
//!   left*. This only produces a sane result when the placeholder was the
//!   sole top-level payload value; with a placeholder nested inside
//!   `{"viewer_sid": "...", "bytes": <placeholder>}` the substitution
//!   leaves behind malformed, un-parseable JSON, and `viewer_sid` is
//!   silently unrecoverable via the public API either way (only the raw
//!   attachment bytes are ever handed to the callback).
//!
//! Because the host side of this protocol must always route by
//! `viewer_sid`, the wrapped shape is load-bearing there, so native binary
//! attachments are unusable for `term:frame`/`term:input` on the host side
//! regardless of sync vs. async client choice (both share this
//! packet/socket code). To keep the wire encoding uniform and the whole
//! relay self-consistent, this module base64-encodes payload bytes into a
//! plain JSON string carried in the `bytes` field for *all* relay traffic
//! in both directions (host and viewer), not just the wrapped host-side
//! events. `term_relay.py` never inspects the contents of `data['bytes']`
//! -- it only checks `is not None` and forwards the value opaquely -- so
//! this substitution is backend-compatible as-is, with no `tanuki_api`
//! changes required.
//!
//! The practical costs of this deviation: relayed frames are ~33% larger
//! on the wire (base64 overhead), and a hypothetical *non-Rust* viewer or
//! host implementation would need to know to base64-decode `bytes` rather
//! than expecting a native binary attachment. If a future maintainer wants
//! true binary attachments here, either upgrade/replace the socket.io
//! client (a client exposing raw packet construction, or a hand-rolled
//! engine.io/socket.io framer) is needed on the Rust side.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use rust_socketio::{ClientBuilder, Payload, RawClient, TransportType};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::ipc::Transport;
use crate::server::client_transport::{self, ServerEvent};

use super::webrtc_p2p::{self, RemoteSignal};

/// P2P negotiation window (Task 5): if a data channel hasn't opened within
/// this long, the caller keeps using the Socket.IO byte-relay indefinitely
/// -- there is no retry, negotiation is a one-shot best-effort upgrade per
/// logical connection.
const P2P_NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Extra (non-STUN) ICE server URLs, e.g. a `turn:` URL once Task 6's
/// `coturn` deployment exists. Empty for now -- P2P only succeeds when a
/// direct host/srflx path exists; symmetric-NAT sessions fall back to the
/// Socket.IO relay until Task 6 lands.
fn cloud_ice_servers() -> Vec<String> {
    Vec::new()
}

/// How long `connect_viewer` waits for `term:attach:ack` / `term:attach:error`
/// before giving up.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);

/// How often `CloudHostTransport` re-sends `term:host_hello` while hosting,
/// to refresh the TTL'd `term:host_sid:{token}` registration on the backend
/// (`tanuki_api/app/socket_handlers/term_relay.py`, `_HOST_TTL_SECONDS`).
/// Must stay comfortably below that TTL so a couple of missed heartbeats
/// (network hiccup) don't expire a still-live registration.
const HOST_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// How long to wait before retrying `term:host_hello` after it was rejected
/// with `host_already_active`. The rejection means the backend still has a
/// (possibly stale) registration for this device token; retrying gives a
/// genuinely dead previous host's entry time to expire (see
/// `HOST_HEARTBEAT_INTERVAL` / backend TTL) instead of leaving this host
/// permanently unregistered after a single rejected attempt.
const HOST_HELLO_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Viewer-side reconnect backoff after an unexpected connection close (i.e.
/// not a graceful `term:detached`). Doubles each failed attempt up to
/// `VIEWER_RECONNECT_MAX_DELAY`. `rust_socketio` 0.6.0 has no built-in
/// reconnect of its own -- `connect()` returns once and the crate's poll
/// thread only keeps an *already open* connection alive -- so this is
/// implemented entirely at this module's level, driven off the `"close"`
/// event.
const VIEWER_RECONNECT_INITIAL_DELAY: Duration = Duration::from_secs(1);
const VIEWER_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(30);

/// Grace window a detached viewer's host-side session (its [`ReplayBuffer`]
/// and pending [`CloudDuplex`]) is kept alive after `term:peer_detached`,
/// before being permanently discarded. Mirrors the backend's own
/// `_HOST_TTL_SECONDS` grace period for the reverse direction (the host's
/// connection dropping, not the viewer's) -- one 30s mental model for
/// "any transient disconnect gets a chance to recover" instead of two
/// different timeouts to reason about.
const VIEWER_SESSION_GRACE_PERIOD: Duration = Duration::from_secs(30);

/// Fixed-size, per-viewer ring buffer of host output bytes with a
/// monotonic sequence counter (a byte-position counter, not a per-message
/// id), so a viewer that reconnects within [`VIEWER_SESSION_GRACE_PERIOD`]
/// can replay exactly what it missed instead of losing it silently. Oldest
/// bytes are evicted first once full -- deliberately bounded (unlike
/// Eternal Terminal's unbounded deque) so a viewer that never reconnects
/// cannot leak host memory indefinitely.
const REPLAY_BUFFER_CAPACITY: usize = 4 * 1024 * 1024;

enum ReplayError {
    /// `from_seq` is older than anything still retained in the buffer --
    /// the caller must fall back to a fresh `term:attach` (accepting the
    /// gap) rather than retry `term:reattach` in a loop.
    TooFarBehind,
}

struct ReplayBuffer {
    data: std::collections::VecDeque<u8>,
    /// Sequence number of the oldest byte still in `data`. A replay
    /// requested from before this point cannot be served.
    start_seq: u64,
    /// Sequence number that will be assigned to the next pushed byte.
    next_seq: u64,
}

impl ReplayBuffer {
    fn new() -> Self {
        Self {
            data: std::collections::VecDeque::with_capacity(REPLAY_BUFFER_CAPACITY),
            start_seq: 0,
            next_seq: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.data.extend(bytes.iter().copied());
        self.next_seq += bytes.len() as u64;
        while self.data.len() > REPLAY_BUFFER_CAPACITY {
            self.data.pop_front();
            self.start_seq += 1;
        }
    }

    /// Returns everything from `from_seq` (inclusive) to `next_seq`
    /// (exclusive), plus the resulting `next_seq` the caller should resume
    /// live-streaming from. `from_seq == next_seq` (viewer was fully
    /// caught up) returns an empty replay, not an error.
    fn replay_from(&self, from_seq: u64) -> Result<(Vec<u8>, u64), ReplayError> {
        if from_seq < self.start_seq {
            return Err(ReplayError::TooFarBehind);
        }
        if from_seq > self.next_seq {
            // Viewer claims to have seen bytes we never sent -- treat the
            // same as "too far behind" rather than trust an inflated
            // client-reported position.
            return Err(ReplayError::TooFarBehind);
        }
        let skip = (from_seq - self.start_seq) as usize;
        let bytes: Vec<u8> = self.data.iter().copied().skip(skip).collect();
        Ok((bytes, self.next_seq))
    }
}

// ---------------------------------------------------------------------------
// Legion device identity (`~/.legion/config.json`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct LegionConfig {
    id: String,
    token: String,
    #[serde(rename = "serverUrl")]
    server_url: String,
}

fn home_dir() -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("USERPROFILE is not set; cannot locate ~/.legion"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("HOME is not set; cannot locate ~/.legion"))
    }
}

fn legion_config_path() -> io::Result<PathBuf> {
    Ok(home_dir()?.join(".legion").join("config.json"))
}

fn load_legion_config() -> io::Result<LegionConfig> {
    let path = legion_config_path()?;
    let bytes = std::fs::read(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "cloud relay: failed to read Legion config at {}: {err}",
                path.display()
            ),
        )
    })?;
    serde_json::from_slice::<LegionConfig>(&bytes).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "cloud relay: Legion config at {} is missing required fields \
                 (id/token/serverUrl): {err}",
                path.display()
            ),
        )
    })
}

fn auth_payload(config: &LegionConfig, role: &str) -> serde_json::Value {
    json!({
        "id": config.id,
        "secret": config.token,
        "type": "legion",
        "role": role,
    })
}

// ---------------------------------------------------------------------------
// bytes <-> wire field encoding (see ARCHITECTURE NOTE above)
// ---------------------------------------------------------------------------

fn encode_bytes_field(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode_bytes_field(value: &serde_json::Value) -> Option<Vec<u8>> {
    let text = value.as_str()?;
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

fn payload_as_value(payload: &Payload) -> Option<serde_json::Value> {
    match payload {
        Payload::Text(values) => values.first().cloned(),
        _ => None,
    }
}

fn extract_str_field(payload: &Payload, field: &str) -> Option<String> {
    payload_as_value(payload)?
        .get(field)?
        .as_str()
        .map(str::to_owned)
}

/// Parses a `{"viewer_sid": "...", "bytes": "<base64>"}` payload, as used by
/// `term:input` (server -> host).
fn extract_viewer_bytes(payload: &Payload) -> Option<(String, Vec<u8>)> {
    let value = payload_as_value(payload)?;
    let viewer_sid = value.get("viewer_sid")?.as_str()?.to_owned();
    let bytes = decode_bytes_field(value.get("bytes")?)?;
    Some((viewer_sid, bytes))
}

/// Parses a bare base64 JSON string payload, as used by `term:frame`
/// (host -> viewer, already addressed to this viewer by the server).
fn extract_bare_bytes(payload: &Payload) -> Option<Vec<u8>> {
    let value = payload_as_value(payload)?;
    decode_bytes_field(&value)
}

// ---------------------------------------------------------------------------
// CloudDuplex: the byte-stream half of one synthesized logical connection
// ---------------------------------------------------------------------------

type WriteFn = dyn Fn(&[u8]) -> io::Result<()> + Send + Sync;

/// Wraps `write_fn` so the actual `.emit(...)` call always runs on a
/// dedicated, runtime-free OS thread, regardless of what thread/async
/// context `CloudDuplex::write` is invoked from.
///
/// `rust_socketio`'s synchronous `Client::emit`/`RawClient::emit` calls its
/// own internal `Runtime::block_on` (see `rust_engineio`'s
/// `websocket_secure::Transport::emit`). If the calling thread is already
/// inside a `block_on` frame -- even indirectly, even via a plain-looking
/// sync fn several calls deep -- that panics with "Cannot start a runtime
/// from within a runtime" (same root cause documented on
/// `CloudHostTransport::spawn` above). `CloudDuplex::write` is reachable
/// from `client::run_client_loop`, which runs inside its own
/// `rt.block_on(...)`, so every `write_fn` must be routed through this
/// wrapper before being handed to `CloudDuplex::new`.
fn detach_write_fn(write_fn: Arc<WriteFn>) -> Arc<WriteFn> {
    let (tx, rx) = std::sync::mpsc::channel::<(Vec<u8>, std::sync::mpsc::Sender<io::Result<()>>)>();
    std::thread::spawn(move || {
        for (data, reply) in rx {
            let _ = reply.send(write_fn(&data));
        }
    });
    Arc::new(move |data: &[u8]| -> io::Result<()> {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        tx.send((data.to_vec(), reply_tx))
            .map_err(|_| io::Error::other("cloud relay: emit worker terminated"))?;
        reply_rx
            .recv()
            .map_err(|_| io::Error::other("cloud relay: emit worker terminated without a result"))?
    })
}

/// Emits `event`/`payload` on `socket` from a dedicated, runtime-free OS
/// thread, fire-and-forget.
///
/// Every `.on(...)` handler registered on a `rust_socketio` client runs
/// from inside the crate's own dispatch loop, which is itself driven by a
/// `block_on` call (see `detach_write_fn`'s doc comment for the underlying
/// `rust_engineio` panic this avoids). Calling `socket.emit(...)` directly
/// from inside such a handler nests a second `block_on` on top of that one
/// and panics with "Cannot start a runtime from within a runtime". The
/// same is true of callbacks invoked from `webrtc_p2p::spawn_negotiation`,
/// which runs on its own dedicated thread that owns its own Tokio runtime
/// (see `webrtc_p2p`'s module doc) -- that thread is just as "ambient" as
/// the dispatch loop's, for this purpose. Route every emit made from
/// inside a handler or a negotiation callback through this helper instead
/// of calling `socket.emit(...)` directly.
fn spawn_socket_emit(socket: RawClient, event: &'static str, payload: serde_json::Value) {
    std::thread::spawn(move || {
        if let Err(err) = socket.emit(event, payload) {
            warn!(event, err = %err, "cloud relay: detached emit failed");
        }
    });
}

/// How long a batched write waits for more bytes to arrive before flushing
/// what it has -- see [`spawn_batched_emit_thread`]. 10ms is imperceptible
/// to a human typing but long enough to coalesce a fast burst of keystrokes
/// (or the several `write()` calls a single pasted line can produce) into
/// one `term:input` emit instead of one per call.
const INPUT_BATCH_WINDOW: Duration = Duration::from_millis(10);
/// Caps how large one coalesced batch can grow before it's flushed early,
/// so a large paste doesn't accumulate for an unbounded number of window
/// refills before anything is sent.
const INPUT_BATCH_MAX_BYTES: usize = 4096;

/// Wraps `emit_fn` so that [`CloudDuplex::write`] calls no longer block on
/// (or synchronously fail from) the network `.emit(...)` call directly --
/// instead each `write()` just hands its bytes to an unbounded channel and
/// returns immediately, and a dedicated background thread coalesces
/// whatever arrives within [`INPUT_BATCH_WINDOW`] (or until
/// [`INPUT_BATCH_MAX_BYTES`] is reached) into a single emit. This targets
/// specifically the viewer -> host `term:input` direction: a raw-mode
/// terminal typically hands `write()` one keystroke at a time, so without
/// this every character was previously its own Socket.IO round trip.
///
/// Byte order is preserved (`buf.extend`, single consumer thread, no
/// reordering) -- `term:frame`/`term:reattach` on the read side are
/// untouched by this, this only wraps the outbound write path.
///
/// Errors from `emit_fn` are logged and dropped rather than surfaced back
/// to the `write()` caller: unlike [`detach_write_fn`], this can no longer
/// return a synchronous `Err` per call. This is safe here specifically
/// because `viewer_reconnect_loop` never gives up and settles into a
/// permanent failure state -- it retries with backoff forever until either
/// a reconnect succeeds or `term:detached` sets `detached` and closes the
/// duplex (which surfaces as EOF on the *read* side, the actual mechanism
/// `ClientError::ConnectionLost` already relies on upstream in
/// `client/mod.rs`). A transient `write()` failure during a reconnect
/// window was never itself the real "connection is dead" signal.
fn spawn_batched_emit_thread(emit_fn: Arc<WriteFn>) -> Arc<WriteFn> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();

    std::thread::spawn(move || {
        while let Ok(first_chunk) = rx.recv() {
            let mut batch = first_chunk;
            let deadline = Instant::now() + INPUT_BATCH_WINDOW;

            while batch.len() < INPUT_BATCH_MAX_BYTES {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                match rx.recv_timeout(deadline - now) {
                    Ok(next_chunk) => batch.extend(next_chunk),
                    Err(_) => break, // window elapsed, or sender side dropped
                }
            }

            if let Err(err) = emit_fn(&batch) {
                warn!(err = %err, "cloud viewer: batched term:input emit failed");
            }
        }
    });

    Arc::new(move |data: &[u8]| -> io::Result<()> {
        // The channel only disconnects once the background thread above has
        // exited, which only happens if `tx` itself was dropped -- can't
        // happen while this closure (which owns a clone-free `tx`) is still
        // alive, so `send` failing here isn't a real-world case; treat it
        // the same as a dropped duplex rather than panicking.
        tx.send(data.to_vec())
            .map_err(|_| io::Error::other("cloud relay: batched emit worker terminated"))
    })
}

struct CloudDuplexState {
    buffer: std::collections::VecDeque<u8>,
    closed: bool,
}

struct CloudDuplexInner {
    state: Mutex<CloudDuplexState>,
    ready: Condvar,
    /// Mutex (not a bare `Arc<WriteFn>`) so a successful P2P negotiation
    /// (Task 5, `remote::webrtc_p2p`) can swap the write path from the
    /// Socket.IO relay emit to the data-channel send, without needing a
    /// new `CloudDuplex`/`Transport` variant. `push`/reads are unaffected
    /// either way -- inbound bytes are handed to `push` regardless of
    /// which transport they arrived over.
    write_fn: Mutex<Arc<WriteFn>>,
    /// Sticky read timeout set via [`CloudDuplex::set_recv_timeout`],
    /// mirroring `SO_RCVTIMEO` semantics on a real socket: `None` blocks
    /// forever, `Some(d)` bounds every subsequent `read()` call until
    /// changed again. Without this, a stuck relay (e.g. a `term:frame`
    /// that never arrives because the server routed `term:peer_attached`
    /// to a dead host sid) hangs the client forever with no error --
    /// exactly the failure mode this exists to turn into a clear
    /// `TimedOut` instead.
    read_timeout: Mutex<Option<Duration>>,
    /// Host-side output replay buffer, `Some` only for a host's per-viewer
    /// `CloudDuplex` (`None` for the viewer's own duplex, which has nothing
    /// to replay to anyone). Lives here -- not inside the Socket.IO-relay
    /// `write_fn` closure -- because [`CloudDuplex::set_write_fn`] swaps
    /// that closure out entirely once a P2P data channel opens
    /// (`webrtc_p2p::on_channel_ready`); pushing from [`Write::write`]
    /// instead means both transports are captured through the one call
    /// site they both funnel through.
    replay: Option<Mutex<ReplayBuffer>>,
}

/// A duplex byte stream backed by a Socket.IO relay connection instead of a
/// real socket. One `CloudDuplex` corresponds to exactly one synthesized
/// logical client connection: on the host side, one per attached viewer; on
/// the viewer side, the single connection to the host.
///
/// `Read` blocks (via condvar) until data pushed by [`CloudDuplex::push`]
/// is available, or the duplex is [`CloudDuplex::close`]d (read then
/// returns `Ok(0)`, i.e. EOF, matching a closed socket). `Write` forwards
/// to the `write_fn` supplied at construction, which performs the actual
/// `.emit(...)` call against the underlying Socket.IO client.
///
/// Cloning shares the same underlying buffer/state (`Arc`-based), matching
/// how `Transport::try_clone` is used elsewhere (writer thread gets its own
/// handle to the same logical connection).
#[derive(Clone)]
pub(crate) struct CloudDuplex {
    inner: Arc<CloudDuplexInner>,
}

impl CloudDuplex {
    fn new(write_fn: Arc<WriteFn>) -> Self {
        Self::new_inner(write_fn, None)
    }

    /// Same as [`CloudDuplex::new`], but with a fresh [`ReplayBuffer`]
    /// attached -- used only for a host's per-viewer duplex.
    fn new_with_replay(write_fn: Arc<WriteFn>) -> Self {
        Self::new_inner(write_fn, Some(Mutex::new(ReplayBuffer::new())))
    }

    fn new_inner(write_fn: Arc<WriteFn>, replay: Option<Mutex<ReplayBuffer>>) -> Self {
        Self {
            inner: Arc::new(CloudDuplexInner {
                state: Mutex::new(CloudDuplexState {
                    buffer: std::collections::VecDeque::new(),
                    closed: false,
                }),
                ready: Condvar::new(),
                write_fn: Mutex::new(write_fn),
                read_timeout: Mutex::new(None),
                replay,
            }),
        }
    }

    /// Replays buffered output from `from_seq` (see [`ReplayBuffer::replay_from`]).
    /// Returns `None` if this duplex has no replay buffer at all (viewer side).
    fn replay_from(&self, from_seq: u64) -> Option<Result<(Vec<u8>, u64), ReplayError>> {
        self.inner.replay.as_ref().map(|replay| {
            replay
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .replay_from(from_seq)
        })
    }

    /// Sets (or clears, with `None`) the sticky read timeout applied to
    /// every subsequent `read()` call. See [`CloudDuplexInner::read_timeout`].
    pub(crate) fn set_recv_timeout(&self, timeout: Option<Duration>) {
        *self
            .inner
            .read_timeout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = timeout;
    }

    /// Swaps the write path, e.g. from the Socket.IO byte-relay emit to a
    /// P2P data-channel send once `remote::webrtc_p2p` negotiation
    /// succeeds. Safe to call concurrently with in-flight `write()` calls;
    /// any write already past the lock acquisition finishes against
    /// whichever `write_fn` it observed, in-flight or subsequent writes
    /// after this call use the new one.
    pub(crate) fn set_write_fn(&self, write_fn: Arc<WriteFn>) {
        *self
            .inner
            .write_fn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = write_fn;
    }

    /// Called from the P2P data-channel's `on_message` callback (see
    /// `remote::webrtc_p2p`) when the write path has been upgraded to P2P
    /// -- feeds inbound bytes into the same buffer `push` from the
    /// Socket.IO relay path uses, so callers never need to know which
    /// transport actually delivered a given chunk.
    pub(crate) fn push_p2p(&self, data: &[u8]) {
        self.push(data);
    }

    /// Called from the Socket.IO event callback thread when a frame
    /// addressed to this logical connection arrives.
    fn push(&self, data: &[u8]) {
        let mut state = self.lock_state();
        if state.closed {
            return;
        }
        state.buffer.extend(data.iter().copied());
        self.inner.ready.notify_all();
    }

    /// Marks the duplex closed, waking any blocked reader with EOF. Mirrors
    /// a peer disconnecting a real socket.
    fn close(&self) {
        let mut state = self.lock_state();
        state.closed = true;
        self.inner.ready.notify_all();
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, CloudDuplexState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Read for CloudDuplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let timeout = *self
            .inner
            .read_timeout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let deadline = timeout.map(|d| std::time::Instant::now() + d);

        let mut state = self.lock_state();
        loop {
            if !state.buffer.is_empty() {
                let n = state.buffer.len().min(buf.len());
                for slot in buf.iter_mut().take(n) {
                    *slot = state.buffer.pop_front().expect("checked non-empty above");
                }
                return Ok(n);
            }
            if state.closed {
                return Ok(0);
            }

            state = match deadline {
                None => self
                    .inner
                    .ready
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
                Some(deadline) => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "cloud relay: read timed out",
                        ));
                    }
                    let (next_state, wait_result) = self
                        .inner
                        .ready
                        .wait_timeout(state, deadline - now)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if wait_result.timed_out() && next_state.buffer.is_empty() && !next_state.closed
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "cloud relay: read timed out",
                        ));
                    }
                    next_state
                }
            };
        }
    }
}

impl Write for CloudDuplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(replay) = &self.inner.replay {
            replay
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(buf);
        }
        let write_fn = Arc::clone(
            &self
                .inner
                .write_fn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        write_fn(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Host side: CloudHostTransport
// ---------------------------------------------------------------------------

/// Manages one Socket.IO connection acting as a tanuki cloud host.
///
/// Kept alive for as long as cloud hosting should stay active. Call
/// [`CloudHostTransport::shutdown`] before dropping to tear the connection
/// down gracefully (`term:host_bye`, heartbeat thread stopped, no
/// auto-reconnect); a bare drop without calling `shutdown` first leaves the
/// heartbeat thread and any in-flight reconnect loop running forever, since
/// neither is owned by this struct's fields (see `host_client_cell` above).
pub(crate) struct CloudHostTransport {
    #[allow(dead_code)]
    client: rust_socketio::client::Client,
    /// Always-current live connection handle, updated by every
    /// `connect_attempt`'s "open" callback (initial connect or reconnect).
    /// `client` above can go stale after a host-side reconnect; `shutdown`
    /// must emit through this cell instead, not `client`, to reach whatever
    /// connection is actually live right now.
    host_client_cell: Arc<Mutex<Option<RawClient>>>,
    /// Set by `shutdown` before emitting `term:host_bye`. Checked by the
    /// heartbeat thread (to stop looping) and by the `"close"` handler (to
    /// skip scheduling a reconnect) so a deliberate toggle-off doesn't
    /// silently reconnect a moment later.
    host_stop: Arc<AtomicBool>,
    viewers: Arc<Mutex<HashMap<String, CloudDuplex>>>,
    /// Same duplexes as `viewers`, indexed by the viewer-generated, sid-independent
    /// `viewer_session_id` instead of the current Socket.IO sid. Needed for
    /// reattach (Задача 10/2): a reconnecting viewer always gets a brand new sid
    /// from the server, so sid alone cannot identify "this is the same logical
    /// session as before." Entries here outlive a `term:peer_detached` for
    /// `VIEWER_SESSION_GRACE_PERIOD` (see `term:peer_detached` handler below),
    /// whereas `viewers` entries are removed only after that same grace window.
    /// Not read through this field directly -- kept alive here only so the
    /// `Arc` (cloned into the connection's closures) doesn't drop early,
    /// same reasoning as the `client` field above.
    #[allow(dead_code)]
    sessions: Arc<Mutex<HashMap<String, CloudDuplex>>>,
}

impl CloudHostTransport {
    /// Connects to the Tanuki backend as a tanuki cloud host and spawns
    /// a new synthesized client connection (via
    /// [`client_transport::handle_client_handshake`]) for every viewer that
    /// attaches, exactly like `client_accept::accept_pending_client_connections`
    /// does for local socket clients.
    ///
    /// `client_id_allocator` must produce a fresh, process-unique client id
    /// on every call (matching the semantics of the existing
    /// `next_client_id` counters in `client_accept.rs` / `headless.rs`).
    pub(crate) fn spawn(
        client_id_allocator: impl Fn() -> u64 + Send + Sync + 'static,
        server_event_tx: mpsc::Sender<ServerEvent>,
        should_quit: Arc<std::sync::atomic::AtomicBool>,
    ) -> io::Result<Self> {
        // Shared for the lifetime of this host, across every reconnect
        // attempt (Задача 2) -- created once here, not inside
        // `connect_attempt`, so a host-side reconnect never loses attached
        // viewers' sessions/replay buffers, unlike a brand new `Self::spawn`
        // would. Mirrors the viewer side's `connect_viewer` hoisting its own
        // per-call state (`viewer_session_id`/`last_ack_seq`/etc.) once,
        // outside `connect_viewer_attempt`.
        let viewers: Arc<Mutex<HashMap<String, CloudDuplex>>> = Arc::new(Mutex::new(HashMap::new()));
        let sessions: Arc<Mutex<HashMap<String, CloudDuplex>>> = Arc::new(Mutex::new(HashMap::new()));
        let sid_to_session: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let detach_epoch: Arc<Mutex<HashMap<String, u64>>> = Arc::new(Mutex::new(HashMap::new()));
        let negotiations: Arc<Mutex<HashMap<String, std::sync::mpsc::Sender<RemoteSignal>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let host_client_cell: Arc<Mutex<Option<RawClient>>> = Arc::new(Mutex::new(None));
        // Guards against spawning overlapping `host_reconnect_loop`s if
        // "close" fires more than once for the same underlying disconnect
        // -- same pattern as the viewer side's `reconnecting`.
        let host_reconnecting = Arc::new(AtomicBool::new(false));
        // Set only by `shutdown` (a deliberate toggle-off), never by a
        // transient network drop -- distinguishes "stop trying to stay
        // connected" from "close" events the heartbeat/reconnect paths
        // should otherwise treat as something to recover from.
        let host_stop = Arc::new(AtomicBool::new(false));
        let client_id_allocator: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(client_id_allocator);

        // Runs for the process lifetime, independent of any single
        // connection attempt -- re-sends `term:host_hello` against whatever
        // connection `host_client_cell` currently points to (updated by
        // every `connect_attempt`'s "open" handler, initial or reconnect),
        // so it survives host-side reconnects without needing to be
        // respawned. Refreshes the TTL'd `term:host_sid:{token}`
        // registration on the backend (`_HOST_TTL_SECONDS`). Exits once
        // `shutdown` sets `host_stop`, so a toggled-off host doesn't keep
        // re-registering itself after `term:host_bye` was already sent.
        {
            let heartbeat_host_client_cell = Arc::clone(&host_client_cell);
            let heartbeat_host_stop = Arc::clone(&host_stop);
            std::thread::spawn(move || loop {
                std::thread::sleep(HOST_HEARTBEAT_INTERVAL);
                if heartbeat_host_stop.load(Ordering::SeqCst) {
                    return;
                }
                let guard = heartbeat_host_client_cell
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let Some(client) = guard.as_ref() else {
                    continue;
                };
                if let Err(err) = client.emit("term:host_hello", json!({})) {
                    warn!(err = %err, "cloud host: failed to send term:host_hello heartbeat");
                }
            });
        }

        // `new_with_cloud_host` (the only caller) runs inside `run_server`'s
        // `rt.block_on(async { ... })`, i.e. already inside a live tokio
        // runtime. `rust_socketio`'s synchronous `ClientBuilder::connect()`
        // internally calls `Runtime::block_on` itself (see
        // `rust_engineio::transports::websocket_secure`), which panics with
        // "Cannot start a runtime from within a runtime" if called while any
        // tokio runtime is already active on the current thread -- even
        // indirectly, even from a plain sync fn. Running the whole connect
        // on a fresh, runtime-free OS thread and blocking on a plain
        // `std::sync::mpsc` channel for the result avoids that: this thread
        // has no ambient tokio context, so `rust_socketio`'s internal
        // `block_on` is free to build its own.
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        {
            let client_id_allocator = Arc::clone(&client_id_allocator);
            let server_event_tx = server_event_tx.clone();
            let should_quit = Arc::clone(&should_quit);
            let viewers = Arc::clone(&viewers);
            let sessions = Arc::clone(&sessions);
            let sid_to_session = Arc::clone(&sid_to_session);
            let detach_epoch = Arc::clone(&detach_epoch);
            let negotiations = Arc::clone(&negotiations);
            let host_client_cell = Arc::clone(&host_client_cell);
            let host_reconnecting = Arc::clone(&host_reconnecting);
            let host_stop = Arc::clone(&host_stop);
            std::thread::spawn(move || {
                let _ = result_tx.send(Self::connect_attempt(
                    client_id_allocator,
                    server_event_tx,
                    should_quit,
                    viewers,
                    sessions,
                    sid_to_session,
                    detach_epoch,
                    negotiations,
                    host_client_cell,
                    host_reconnecting,
                    host_stop,
                ));
            });
        }
        let client = result_rx.recv().map_err(|_| {
            io::Error::other("cloud host: connect thread terminated without a result")
        })??;

        Ok(Self {
            client,
            host_client_cell,
            host_stop,
            viewers,
            sessions,
        })
    }

    /// Gracefully stops cloud hosting: marks `host_stop` so the heartbeat
    /// thread exits and the `"close"` handler doesn't schedule a
    /// reconnect, then sends `term:host_bye` on whatever connection is
    /// currently live (`term_relay.py`'s `on_term_host_bye` tears down the
    /// backend-side registration and notifies attached viewers). Does not
    /// itself drop the transport -- the caller drops it afterwards once
    /// this returns, at which point `client`'s own `Drop` closes the
    /// socket.
    pub(crate) fn shutdown(&self) {
        self.host_stop.store(true, Ordering::SeqCst);
        let guard = self
            .host_client_cell
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(client) = guard.as_ref() {
            if let Err(err) = client.emit("term:host_bye", json!({})) {
                warn!(err = %err, "cloud host: failed to send term:host_bye on shutdown");
            }
        }
    }

    /// One connection attempt -- used both for the initial connect (from
    /// [`spawn`]) and for every subsequent reconnect (from
    /// [`host_reconnect_loop`], driven off the `"close"` handler installed
    /// below). All the `Arc`-shared maps are passed in rather than created
    /// here, so viewers/replay buffers/negotiations survive a host-side
    /// reconnect intact -- only the underlying Socket.IO `Client` is new.
    #[allow(clippy::too_many_arguments)]
    fn connect_attempt(
        client_id_allocator: Arc<dyn Fn() -> u64 + Send + Sync>,
        server_event_tx: mpsc::Sender<ServerEvent>,
        should_quit: Arc<std::sync::atomic::AtomicBool>,
        viewers: Arc<Mutex<HashMap<String, CloudDuplex>>>,
        sessions: Arc<Mutex<HashMap<String, CloudDuplex>>>,
        sid_to_session: Arc<Mutex<HashMap<String, String>>>,
        detach_epoch: Arc<Mutex<HashMap<String, u64>>>,
        negotiations: Arc<Mutex<HashMap<String, std::sync::mpsc::Sender<RemoteSignal>>>>,
        host_client_cell: Arc<Mutex<Option<RawClient>>>,
        host_reconnecting: Arc<AtomicBool>,
        host_stop: Arc<AtomicBool>,
    ) -> io::Result<rust_socketio::client::Client> {
        let config = load_legion_config()?;
        let auth = auth_payload(&config, "term-host");

        let attach_viewers = Arc::clone(&viewers);
        let attach_sessions = Arc::clone(&sessions);
        let attach_sid_to_session = Arc::clone(&sid_to_session);
        let attach_client_id_allocator = Arc::clone(&client_id_allocator);
        let attach_server_event_tx = server_event_tx.clone();
        let attach_should_quit = Arc::clone(&should_quit);
        let attach_negotiations = Arc::clone(&negotiations);

        let detach_viewers = Arc::clone(&viewers);
        let detach_sessions = Arc::clone(&sessions);
        let detach_sid_to_session = Arc::clone(&sid_to_session);
        let detach_epoch = Arc::clone(&detach_epoch);
        let detach_negotiations = Arc::clone(&negotiations);
        let input_viewers = Arc::clone(&viewers);
        let offer_negotiations = Arc::clone(&negotiations);
        let ice_negotiations = Arc::clone(&negotiations);
        let reattach_sessions = Arc::clone(&sessions);
        let reattach_viewers = Arc::clone(&viewers);
        let reattach_sid_to_session = Arc::clone(&sid_to_session);

        // `host_client_cell` is now a parameter (shared across reconnects,
        // populated by the heartbeat loop spawned once in `spawn`) --
        // just clone it for the callbacks below that need their own handle.
        let open_host_client_cell = Arc::clone(&host_client_cell);
        let retry_host_client_cell = Arc::clone(&host_client_cell);

        let close_client_id_allocator = Arc::clone(&client_id_allocator);
        let close_server_event_tx = server_event_tx.clone();
        let close_should_quit = Arc::clone(&should_quit);
        let close_viewers = Arc::clone(&viewers);
        let close_sessions = Arc::clone(&sessions);
        let close_sid_to_session = Arc::clone(&sid_to_session);
        let close_detach_epoch = Arc::clone(&detach_epoch);
        let close_negotiations = Arc::clone(&negotiations);
        let close_host_client_cell = Arc::clone(&host_client_cell);
        let close_host_reconnecting = Arc::clone(&host_reconnecting);
        let close_host_stop = Arc::clone(&host_stop);

        let client = ClientBuilder::new(config.server_url.clone())
            .transport_type(TransportType::Websocket)
            .auth(auth)
            .on("open", move |_payload, socket: RawClient| {
                *open_host_client_cell
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(socket.clone());
                spawn_socket_emit(socket, "term:host_hello", json!({}));
            })
            .on("term:host_hello:ack", |_payload, _socket| {
                debug!("cloud host: registered with relay server");
            })
            .on("term:host_hello:error", move |payload, _socket| {
                let reason =
                    extract_str_field(&payload, "reason").unwrap_or_else(|| "unknown".to_owned());
                warn!(
                    reason = %reason,
                    "cloud host: term:host_hello rejected, retrying in {:?}",
                    HOST_HELLO_RETRY_DELAY
                );
                let host_client_cell = Arc::clone(&retry_host_client_cell);
                std::thread::spawn(move || {
                    std::thread::sleep(HOST_HELLO_RETRY_DELAY);
                    let guard = host_client_cell
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let Some(client) = guard.as_ref() {
                        if let Err(err) = client.emit("term:host_hello", json!({})) {
                            warn!(err = %err, "cloud host: failed to retry term:host_hello");
                        }
                    }
                });
            })
            .on("term:peer_attached", move |payload, socket: RawClient| {
                let Some(viewer_sid) = extract_str_field(&payload, "viewer_sid") else {
                    warn!("cloud host: term:peer_attached missing viewer_sid");
                    return;
                };
                // Opaque, viewer-generated id (see `sessions` field doc):
                // `rust_socketio` 0.6.0 exposes no way for a client to learn
                // its own sid, and sid changes on every reconnect anyway, so
                // this is the only stable handle a viewer can present again
                // on `term:reattach`. Older viewers that predate Задача 10
                // won't send one -- fall back to the sid itself so such a
                // viewer still gets a (non-reattachable, but otherwise
                // correct) session; it simply can never be found by a
                // `term:reattach` lookup, which is the pre-existing behavior.
                let viewer_session_id = extract_str_field(&payload, "viewer_session_id")
                    .unwrap_or_else(|| viewer_sid.clone());

                // Cloned before `socket` is moved into `write_fn` below --
                // reused by the P2P negotiation block further down, which
                // needs its own handle to emit signaling events.
                let p2p_socket = socket.clone();

                let emit_viewer_sid = viewer_sid.clone();
                let write_fn: Arc<WriteFn> = Arc::new(move |data: &[u8]| -> io::Result<()> {
                    socket
                        .emit(
                            "term:frame",
                            json!({
                                "viewer_sid": emit_viewer_sid,
                                "bytes": encode_bytes_field(data),
                            }),
                        )
                        .map_err(|err| io::Error::other(err.to_string()))
                });
                let duplex = CloudDuplex::new_with_replay(detach_write_fn(write_fn));

                attach_viewers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(viewer_sid.clone(), duplex.clone());
                attach_sessions
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(viewer_session_id.clone(), duplex.clone());
                attach_sid_to_session
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(viewer_sid.clone(), viewer_session_id);

                // Best-effort P2P upgrade (Task 5): the host is the
                // answerer, since the viewer is the side that initiated
                // `term:attach`. Failure/timeout leaves the Socket.IO
                // relay path above as the permanent transport for this
                // viewer -- no retry.
                {
                    let emit_answer: Arc<dyn Fn(&str) + Send + Sync> = {
                        let socket = p2p_socket.clone();
                        let viewer_sid = viewer_sid.clone();
                        Arc::new(move |sdp: &str| {
                            spawn_socket_emit(
                                socket.clone(),
                                "term:webrtc_answer",
                                json!({"viewer_sid": viewer_sid, "sdp": sdp}),
                            );
                        })
                    };
                    let emit_ice: Arc<dyn Fn(&str) + Send + Sync> = {
                        let socket = p2p_socket.clone();
                        let viewer_sid = viewer_sid.clone();
                        Arc::new(move |candidate: &str| {
                            // `candidate` is already the JSON body of an
                            // `RTCIceCandidateInit` (see `webrtc_p2p`'s
                            // `on_ice_candidate`). It must be parsed back
                            // into a `Value` before nesting it here --
                            // `json!({"candidate": candidate})` would embed
                            // it as a JSON *string* (double-encoded),
                            // which the receiving `serde_json::from_str::
                            // <RTCIceCandidateInit>` then rejects as
                            // "invalid type: string ... expected struct".
                            let Ok(candidate) = serde_json::from_str::<serde_json::Value>(candidate)
                            else {
                                warn!("cloud host: failed to parse locally-gathered ICE candidate JSON");
                                return;
                            };
                            spawn_socket_emit(
                                socket.clone(),
                                "term:webrtc_ice",
                                json!({"viewer_sid": viewer_sid, "candidate": candidate}),
                            );
                        })
                    };
                    let emit_offer_unused: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_sdp: &str| {
                        debug_assert!(false, "cloud host is always the P2P answerer, never emits an offer");
                    });
                    let on_channel_ready: Arc<dyn Fn(Arc<WriteFn>) + Send + Sync> = {
                        let duplex = duplex.clone();
                        let viewer_sid = viewer_sid.clone();
                        Arc::new(move |write_fn| {
                            duplex.set_write_fn(write_fn);
                            debug!(viewer_sid = %viewer_sid, "cloud host: P2P data channel established, switched off relay");
                        })
                    };
                    let on_data: webrtc_p2p::DataCallback = {
                        let duplex = duplex.clone();
                        Arc::new(move |data: &[u8]| duplex.push_p2p(data))
                    };
                    // Task 7: report the negotiation outcome so the
                    // backend can track % direct-P2P vs TURN-relay
                    // sessions (pre-mortem Track Tiger #6).
                    let on_outcome: Arc<dyn Fn(webrtc_p2p::NegotiationOutcome) + Send + Sync> = {
                        let socket = p2p_socket.clone();
                        let viewer_sid = viewer_sid.clone();
                        Arc::new(move |outcome: webrtc_p2p::NegotiationOutcome| {
                            spawn_socket_emit(
                                socket.clone(),
                                "term:webrtc_stats",
                                json!({
                                    "viewer_sid": viewer_sid,
                                    "established": outcome.established,
                                    "candidate_type": outcome.best_local_candidate_type.unwrap_or("unknown"),
                                }),
                            );
                        })
                    };

                    let remote_tx = webrtc_p2p::spawn_negotiation(
                        false,
                        cloud_ice_servers(),
                        emit_offer_unused,
                        emit_answer,
                        emit_ice,
                        on_channel_ready,
                        on_data,
                        on_outcome,
                        P2P_NEGOTIATION_TIMEOUT,
                    );
                    attach_negotiations
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(viewer_sid.clone(), remote_tx);
                }

                let client_id = (attach_client_id_allocator)();
                let server_event_tx = attach_server_event_tx.clone();
                let should_quit = attach_should_quit.clone();
                debug!(client_id, viewer_sid = %viewer_sid, "cloud host: viewer attached");
                std::thread::spawn(move || {
                    if let Err(err) = client_transport::handle_client_handshake(
                        Transport::Cloud(duplex),
                        client_id,
                        &server_event_tx,
                        &should_quit,
                    ) {
                        debug!(client_id, err = %err, "cloud host: viewer handshake failed");
                    }
                });
            })
            .on("term:peer_detached", move |payload, _socket| {
                let Some(viewer_sid) = extract_str_field(&payload, "viewer_sid") else {
                    return;
                };
                // Stop routing to this (now-dead) sid immediately -- but do
                // NOT close the duplex or drop it from `sessions` yet. The
                // local client_transport thread reading/writing through it
                // just blocks (same as a slow/idle viewer) until either a
                // `term:reattach` reclaims it or the grace window below
                // expires; closing it here would EOF that thread and tear
                // down whatever local session state this viewer's synthesized
                // client connection represents -- exactly the data-loss
                // Задача 10 exists to prevent.
                detach_viewers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&viewer_sid);
                // Dropping the sender lets `webrtc_p2p::spawn_negotiation`'s
                // forwarding thread observe closure and exit if negotiation
                // for this viewer was still in flight. A reattach does not
                // resume P2P -- it permanently falls back to the Socket.IO
                // relay for the remainder of that logical session (no
                // renegotiation attempted), which is an accepted, documented
                // degradation for now.
                detach_negotiations
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&viewer_sid);

                let Some(viewer_session_id) = detach_sid_to_session
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&viewer_sid)
                else {
                    return;
                };

                let epoch = {
                    let mut guard = detach_epoch
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let epoch = guard.entry(viewer_session_id.clone()).or_insert(0);
                    *epoch += 1;
                    *epoch
                };

                let sessions = Arc::clone(&detach_sessions);
                let detach_epoch = Arc::clone(&detach_epoch);
                std::thread::spawn(move || {
                    std::thread::sleep(VIEWER_SESSION_GRACE_PERIOD);
                    let still_pending = {
                        let guard = detach_epoch
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        guard.get(&viewer_session_id) == Some(&epoch)
                    };
                    if !still_pending {
                        // Reattached (or detached again) since this timer
                        // was scheduled -- someone else now owns this
                        // session's lifecycle, leave it alone.
                        return;
                    }
                    if let Some(duplex) = sessions
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&viewer_session_id)
                    {
                        duplex.close();
                    }
                });
            })
            .on("term:peer_reattached", move |payload, socket: RawClient| {
                let Some(value) = payload_as_value(&payload) else {
                    return;
                };
                let Some(new_viewer_sid) = value.get("viewer_sid").and_then(|v| v.as_str())
                else {
                    return;
                };
                let Some(viewer_session_id) =
                    value.get("viewer_session_id").and_then(|v| v.as_str())
                else {
                    return;
                };
                let Some(last_seq) = value.get("last_seq").and_then(|v| v.as_u64()) else {
                    return;
                };

                let duplex = reattach_sessions
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(viewer_session_id)
                    .cloned();

                let Some(duplex) = duplex else {
                    spawn_socket_emit(
                        socket,
                        "term:reattach_error",
                        json!({"viewer_sid": new_viewer_sid, "reason": "unknown_viewer"}),
                    );
                    return;
                };

                match duplex.replay_from(last_seq) {
                    None => {
                        // Should be unreachable -- every entry in `sessions`
                        // was created via `new_with_replay` -- but fail
                        // closed rather than panic if it ever happens.
                        spawn_socket_emit(
                            socket,
                            "term:reattach_error",
                            json!({"viewer_sid": new_viewer_sid, "reason": "unknown_viewer"}),
                        );
                    }
                    Some(Err(ReplayError::TooFarBehind)) => {
                        spawn_socket_emit(
                            socket,
                            "term:reattach_error",
                            json!({"viewer_sid": new_viewer_sid, "reason": "too_far_behind"}),
                        );
                    }
                    Some(Ok((replay_bytes, resume_seq))) => {
                        // Rebuild the write path against the NEW sid --
                        // the old `write_fn` closure still addresses the
                        // now-dead sid from the original attach and would
                        // silently deliver nowhere. Reuses `set_write_fn`,
                        // the same mechanism the P2P upgrade path uses.
                        let new_sid = new_viewer_sid.to_owned();
                        let write_socket = socket.clone();
                        let write_fn: Arc<WriteFn> = Arc::new(move |data: &[u8]| -> io::Result<()> {
                            write_socket
                                .emit(
                                    "term:frame",
                                    json!({
                                        "viewer_sid": new_sid,
                                        "bytes": encode_bytes_field(data),
                                    }),
                                )
                                .map_err(|err| io::Error::other(err.to_string()))
                        });
                        duplex.set_write_fn(detach_write_fn(write_fn));

                        reattach_viewers
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .insert(new_viewer_sid.to_owned(), duplex);
                        reattach_sid_to_session
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .insert(new_viewer_sid.to_owned(), viewer_session_id.to_owned());

                        spawn_socket_emit(
                            socket,
                            "term:reattach_ack",
                            json!({
                                "viewer_sid": new_viewer_sid,
                                "replay_bytes": encode_bytes_field(&replay_bytes),
                                "resume_seq": resume_seq,
                            }),
                        );

                        debug!(
                            viewer_session_id = %viewer_session_id,
                            new_viewer_sid = %new_viewer_sid,
                            resume_seq,
                            replay_len = replay_bytes.len(),
                            "cloud host: viewer reattached, replaying buffered output"
                        );
                    }
                }
            })
            .on("term:webrtc_offer", move |payload, _socket| {
                let Some(value) = payload_as_value(&payload) else {
                    return;
                };
                let Some(viewer_sid) = value.get("viewer_sid").and_then(|v| v.as_str()) else {
                    return;
                };
                let Some(sdp) = value.get("sdp").and_then(|v| v.as_str()) else {
                    return;
                };
                let guard = offer_negotiations
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(tx) = guard.get(viewer_sid) {
                    let _ = tx.send(RemoteSignal::Offer(sdp.to_owned()));
                }
            })
            .on("term:webrtc_ice", move |payload, _socket| {
                let Some(value) = payload_as_value(&payload) else {
                    return;
                };
                let Some(viewer_sid) = value.get("viewer_sid").and_then(|v| v.as_str()) else {
                    return;
                };
                let Some(candidate) = value.get("candidate") else {
                    return;
                };
                let Ok(candidate_json) = serde_json::to_string(candidate) else {
                    return;
                };
                let guard = ice_negotiations
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(tx) = guard.get(viewer_sid) {
                    let _ = tx.send(RemoteSignal::IceCandidate(candidate_json));
                }
            })
            .on("term:input", move |payload, _socket| {
                let Some((viewer_sid, bytes)) = extract_viewer_bytes(&payload) else {
                    warn!("cloud host: term:input missing/malformed viewer_sid or bytes");
                    return;
                };
                let viewers = input_viewers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(duplex) = viewers.get(&viewer_sid) {
                    duplex.push(&bytes);
                } else {
                    debug!(viewer_sid = %viewer_sid, "cloud host: term:input for unknown viewer");
                }
            })
            .on("error", |payload, _socket| {
                warn!(payload = ?payload, "cloud host: socket.io error");
            })
            .on("close", move |payload, _socket| {
                if close_host_stop.load(Ordering::SeqCst) {
                    // Deliberate shutdown (see CloudHostTransport::shutdown) --
                    // this close was expected, do not reconnect.
                    return;
                }
                if close_host_reconnecting.swap(true, Ordering::SeqCst) {
                    // A reconnect loop from a previous "close" is already running.
                    return;
                }
                warn!(
                    payload = ?payload,
                    "cloud host: connection closed unexpectedly, reconnecting"
                );
                let client_id_allocator = Arc::clone(&close_client_id_allocator);
                let server_event_tx = close_server_event_tx.clone();
                let should_quit = Arc::clone(&close_should_quit);
                let viewers = Arc::clone(&close_viewers);
                let sessions = Arc::clone(&close_sessions);
                let sid_to_session = Arc::clone(&close_sid_to_session);
                let detach_epoch = Arc::clone(&close_detach_epoch);
                let negotiations = Arc::clone(&close_negotiations);
                let host_client_cell = Arc::clone(&close_host_client_cell);
                let host_reconnecting = Arc::clone(&close_host_reconnecting);
                let host_stop = Arc::clone(&close_host_stop);
                std::thread::spawn(move || {
                    host_reconnect_loop(
                        client_id_allocator,
                        server_event_tx,
                        should_quit,
                        viewers,
                        sessions,
                        sid_to_session,
                        detach_epoch,
                        negotiations,
                        host_client_cell,
                        host_reconnecting,
                        host_stop,
                    );
                });
            })
            .connect()
            .map_err(|err| {
                io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("cloud host: connect failed: {err}"),
                )
            })?;

        Ok(client)
    }

    /// Number of viewers currently attached to this host session.
    pub(crate) fn attached_viewer_count(&self) -> usize {
        self.viewers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

/// Runs after an unexpected host-side `"close"` (the host's own socket to
/// the backend dropped -- not a graceful shutdown, which goes through
/// `term:host_bye` and doesn't touch this transport at all). Retries
/// [`CloudHostTransport::connect_attempt`] with exponential backoff,
/// reusing the same `viewers`/`sessions`/etc. `Arc`s so attached viewers'
/// replay buffers survive the drop -- mirrors [`viewer_reconnect_loop`].
#[allow(clippy::too_many_arguments)]
fn host_reconnect_loop(
    client_id_allocator: Arc<dyn Fn() -> u64 + Send + Sync>,
    server_event_tx: mpsc::Sender<ServerEvent>,
    should_quit: Arc<AtomicBool>,
    viewers: Arc<Mutex<HashMap<String, CloudDuplex>>>,
    sessions: Arc<Mutex<HashMap<String, CloudDuplex>>>,
    sid_to_session: Arc<Mutex<HashMap<String, String>>>,
    detach_epoch: Arc<Mutex<HashMap<String, u64>>>,
    negotiations: Arc<Mutex<HashMap<String, std::sync::mpsc::Sender<RemoteSignal>>>>,
    host_client_cell: Arc<Mutex<Option<RawClient>>>,
    host_reconnecting: Arc<AtomicBool>,
    host_stop: Arc<AtomicBool>,
) {
    let mut delay = VIEWER_RECONNECT_INITIAL_DELAY;
    loop {
        std::thread::sleep(delay);

        if host_stop.load(Ordering::SeqCst) {
            // Shutdown requested while a retry was in flight/sleeping --
            // stop trying to reconnect.
            host_reconnecting.store(false, Ordering::SeqCst);
            return;
        }

        let attempt = CloudHostTransport::connect_attempt(
            Arc::clone(&client_id_allocator),
            server_event_tx.clone(),
            Arc::clone(&should_quit),
            Arc::clone(&viewers),
            Arc::clone(&sessions),
            Arc::clone(&sid_to_session),
            Arc::clone(&detach_epoch),
            Arc::clone(&negotiations),
            Arc::clone(&host_client_cell),
            Arc::clone(&host_reconnecting),
            Arc::clone(&host_stop),
        );

        match attempt {
            Ok(client) => {
                drop(client);
                warn!("cloud host: reconnected after unexpected close");
                host_reconnecting.store(false, Ordering::SeqCst);
                return;
            }
            Err(err) => {
                warn!(
                    err = %err,
                    delay = ?delay,
                    "cloud host: reconnect attempt failed, retrying"
                );
                delay = (delay * 2).min(VIEWER_RECONNECT_MAX_DELAY);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Viewer side: connect_viewer
// ---------------------------------------------------------------------------

type AttachResult = Result<(), String>;

/// Opaque per-connect_viewer-call correlation id, not a security token --
/// only needs to be practically unique among concurrent viewer sessions
/// from this device, which PID + nanosecond timestamp + a process-local
/// counter already guarantees without pulling in a `uuid`/`rand`
/// dependency for this alone.
fn generate_viewer_session_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{:x}-{:x}", std::process::id(), nanos, counter)
}

/// Connects to the Tanuki backend as a tanuki cloud viewer attaching to
/// `target_token_id` (another of the caller's own paired devices), and
/// blocks until the attach either succeeds or is rejected.
///
/// On success, returns a [`Transport::Cloud`] that behaves like any other
/// `Transport` -- callers use it exactly where they would otherwise use
/// `Transport::Local(crate::ipc::connect_local_stream(...)?)`.
pub(crate) fn connect_viewer(target_token_id: &str) -> io::Result<Transport> {
    let config = load_legion_config()?;
    let target_token_id = target_token_id.to_owned();

    // Opaque, stable for the lifetime of this `connect_viewer` call (i.e.
    // across every reconnect attempt below) -- see the matching doc on
    // `CloudHostTransport::sessions` for why this exists instead of relying
    // on the Socket.IO sid (which `rust_socketio` never exposes to the
    // client anyway, and which changes on every reconnect regardless).
    let viewer_session_id = generate_viewer_session_id();
    // Running byte-position counter, advanced by every `term:frame` this
    // connection receives. Used as `last_seq` on a `term:reattach` after a
    // reconnect, so the host can replay only what was actually missed.
    let last_ack_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
    // False until the first successful `term:attach:ack`/`term:reattach_ack`
    // -- controls whether the *next* "open" (including the very first one)
    // emits `term:attach` (nothing to resume yet) or `term:reattach`
    // (resume from `last_ack_seq`).
    let has_attached_before = Arc::new(AtomicBool::new(false));

    // The Socket.IO client handle needed to emit `term:input` at write-time
    // (from arbitrary caller threads, outside any event callback) is only
    // available once `.connect()` returns below. Every `on(...)` callback
    // receives its own `RawClient` handle immediately though, so we grab a
    // copy of it from the very first callback that fires ("open") and stash
    // it here for the write closure to use afterwards. On reconnect (see
    // `viewer_reconnect_loop`) this same cell gets overwritten with the new
    // client, so in-flight writes always target whichever connection is
    // currently live.
    let client_cell: Arc<Mutex<Option<RawClient>>> = Arc::new(Mutex::new(None));

    let write_client_cell = Arc::clone(&client_cell);
    let write_fn: Arc<WriteFn> = Arc::new(move |data: &[u8]| -> io::Result<()> {
        let guard = write_client_cell
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(client) = guard.as_ref() else {
            return Err(io::Error::other("cloud viewer: not connected yet"));
        };
        client
            .emit("term:input", json!(encode_bytes_field(data)))
            .map_err(|err| io::Error::other(err.to_string()))
    });
    // Batched, not detach_write_fn: this is specifically the viewer's own
    // outbound term:input path (see spawn_batched_emit_thread's doc comment
    // for why coalescing keystrokes here doesn't need the per-call blocking
    // round trip detach_write_fn gives the host's term:frame writes).
    let duplex = CloudDuplex::new(spawn_batched_emit_thread(write_fn));

    let attach_state: Arc<(Mutex<Option<AttachResult>>, Condvar)> =
        Arc::new((Mutex::new(None), Condvar::new()));
    // Set once `term:detached` is received (the host cleanly ended the
    // session) so the `"close"` handler that follows it knows not to
    // reconnect -- closing was intentional, not a dropped connection.
    let detached = Arc::new(AtomicBool::new(false));
    // Set while `term:suspended` is the most recent host-state event (the
    // HOST's own connection to the relay dropped, not this viewer's --
    // Задача 2 grace period). Distinct from `detached`: the duplex stays
    // open and reads keep blocking exactly as if the host were just slow,
    // never surfacing an EOF to the local terminal. Cleared the moment a
    // `term:frame` arrives again, since that's proof the host is back.
    // Not read by the reconnect path itself -- reattach-on-reconnect
    // already happens unconditionally once `has_attached_before` is set
    // (Задача 10) -- this flag exists for observability/future UI use.
    let suspended = Arc::new(AtomicBool::new(false));
    // Guards against spawning overlapping reconnect loops if `"close"`
    // fires more than once for the same underlying disconnect.
    let reconnecting = Arc::new(AtomicBool::new(false));

    // Holds the currently-active P2P negotiation's signaling sender (Task
    // 5), so the `term:webrtc_answer`/`term:webrtc_ice` handlers know
    // where to forward incoming signaling messages. Re-populated on every
    // successful attach (initial or reconnect) -- see the `term:attach:ack`
    // handler in `connect_viewer_attempt`.
    let p2p_signal_cell: Arc<Mutex<Option<std::sync::mpsc::Sender<RemoteSignal>>>> =
        Arc::new(Mutex::new(None));

    let client = connect_viewer_attempt(
        &config,
        target_token_id,
        Arc::clone(&client_cell),
        duplex.clone(),
        Arc::clone(&attach_state),
        Arc::clone(&detached),
        Arc::clone(&suspended),
        Arc::clone(&reconnecting),
        Arc::clone(&p2p_signal_cell),
        viewer_session_id.clone(),
        Arc::clone(&last_ack_seq),
        Arc::clone(&has_attached_before),
    )?;

    // The connection is kept alive by the crate's own background poll
    // thread (which holds its own client reference), not by this local
    // binding, so it is fine to let `client` go out of scope once we are
    // done using it here.
    drop(client);

    wait_for_attach_result(&attach_state)?;

    Ok(Transport::Cloud(duplex))
}

/// Builds and connects one viewer-side Socket.IO client, wiring its
/// callbacks against the shared state (`client_cell`/`duplex`/`attach_state`)
/// so that both the very first connection attempt in [`connect_viewer`] and
/// every subsequent reconnect attempt in [`viewer_reconnect_loop`] behave
/// identically from the caller's (and the local terminal's) point of view.
/// Best-effort P2P upgrade (Task 5), shared between the plain-attach and
/// reattach success paths -- the viewer is always the offerer, since it's
/// always the side that just (re)initiated the attach. Failure/timeout
/// leaves the Socket.IO relay path (already live via the ack that preceded
/// this call) as the permanent transport for this attach -- no retry.
fn spawn_viewer_p2p_negotiation(
    socket: RawClient,
    duplex: CloudDuplex,
    p2p_signal_cell: Arc<Mutex<Option<std::sync::mpsc::Sender<RemoteSignal>>>>,
) {
    let emit_offer: Arc<dyn Fn(&str) + Send + Sync> = {
        let socket = socket.clone();
        Arc::new(move |sdp: &str| {
            spawn_socket_emit(socket.clone(), "term:webrtc_offer", json!({"sdp": sdp}));
        })
    };
    let emit_ice: Arc<dyn Fn(&str) + Send + Sync> = {
        let socket = socket.clone();
        Arc::new(move |candidate: &str| {
            // See the host-side `emit_ice`'s doc comment in
            // `connect_attempt` -- `candidate` is already serialized
            // `RTCIceCandidateInit` JSON and must be parsed back into a
            // `Value` before nesting, or it gets double-encoded as a
            // string.
            let Ok(candidate) = serde_json::from_str::<serde_json::Value>(candidate) else {
                warn!("cloud viewer: failed to parse locally-gathered ICE candidate JSON");
                return;
            };
            spawn_socket_emit(
                socket.clone(),
                "term:webrtc_ice",
                json!({"candidate": candidate}),
            );
        })
    };
    let emit_answer_unused: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_sdp: &str| {
        debug_assert!(false, "cloud viewer is always the P2P offerer, never emits an answer");
    });
    let on_channel_ready: Arc<dyn Fn(Arc<WriteFn>) + Send + Sync> = {
        let duplex = duplex.clone();
        Arc::new(move |write_fn| {
            duplex.set_write_fn(write_fn);
            debug!("cloud viewer: P2P data channel established, switched off relay");
        })
    };
    let on_data: webrtc_p2p::DataCallback = {
        let duplex = duplex.clone();
        Arc::new(move |data: &[u8]| duplex.push_p2p(data))
    };
    // Task 7 telemetry -- no `viewer_sid` field needed, the server already
    // knows this connection's own sid from the emitting socket itself.
    let on_outcome: Arc<dyn Fn(webrtc_p2p::NegotiationOutcome) + Send + Sync> = {
        let socket = socket.clone();
        Arc::new(move |outcome: webrtc_p2p::NegotiationOutcome| {
            spawn_socket_emit(
                socket.clone(),
                "term:webrtc_stats",
                json!({
                    "established": outcome.established,
                    "candidate_type": outcome.best_local_candidate_type.unwrap_or("unknown"),
                }),
            );
        })
    };

    let remote_tx = webrtc_p2p::spawn_negotiation(
        true,
        cloud_ice_servers(),
        emit_offer,
        emit_answer_unused,
        emit_ice,
        on_channel_ready,
        on_data,
        on_outcome,
        P2P_NEGOTIATION_TIMEOUT,
    );
    *p2p_signal_cell
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(remote_tx);
}

// Reattach/suspend state (Задача 10/2) pushed this past clippy's configured
// too-many-arguments-threshold=11; grouping into a struct would touch every
// call site for marginal gain over this explicit, compiler-suggested override.
#[allow(clippy::too_many_arguments)]
fn connect_viewer_attempt(
    config: &LegionConfig,
    target_token_id: String,
    client_cell: Arc<Mutex<Option<RawClient>>>,
    duplex: CloudDuplex,
    attach_state: Arc<(Mutex<Option<AttachResult>>, Condvar)>,
    detached: Arc<AtomicBool>,
    suspended: Arc<AtomicBool>,
    reconnecting: Arc<AtomicBool>,
    p2p_signal_cell: Arc<Mutex<Option<std::sync::mpsc::Sender<RemoteSignal>>>>,
    viewer_session_id: String,
    last_ack_seq: Arc<std::sync::atomic::AtomicU64>,
    has_attached_before: Arc<AtomicBool>,
) -> io::Result<rust_socketio::client::Client> {
    let auth = auth_payload(config, "term-viewer");

    let open_client_cell = Arc::clone(&client_cell);
    let open_attach_state = Arc::clone(&attach_state);
    let open_viewer_session_id = viewer_session_id.clone();
    let open_last_ack_seq = Arc::clone(&last_ack_seq);
    let open_has_attached_before = Arc::clone(&has_attached_before);
    let ack_attach_state = Arc::clone(&attach_state);
    let ack_duplex = duplex.clone();
    let ack_p2p_signal_cell = Arc::clone(&p2p_signal_cell);
    let ack_has_attached_before = Arc::clone(&has_attached_before);
    let ack_last_ack_seq = Arc::clone(&last_ack_seq);
    let error_attach_state = Arc::clone(&attach_state);
    let reattach_ack_attach_state = Arc::clone(&attach_state);
    let reattach_ack_duplex = duplex.clone();
    let reattach_ack_p2p_signal_cell = Arc::clone(&p2p_signal_cell);
    let reattach_ack_has_attached_before = Arc::clone(&has_attached_before);
    let reattach_ack_last_ack_seq = Arc::clone(&last_ack_seq);
    let reattach_error_target_token_id = target_token_id.clone();
    let reattach_error_viewer_session_id = viewer_session_id.clone();
    let reattach_error_attach_state = Arc::clone(&attach_state);
    let frame_duplex = duplex.clone();
    let frame_last_ack_seq = Arc::clone(&last_ack_seq);
    let frame_suspended = Arc::clone(&suspended);
    let suspended_flag = Arc::clone(&suspended);
    let detach_duplex = duplex.clone();
    let detach_detached = Arc::clone(&detached);
    let detach_suspended = Arc::clone(&suspended);
    let open_target_token_id = target_token_id.clone();
    let ice_p2p_signal_cell = Arc::clone(&p2p_signal_cell);
    let answer_p2p_signal_cell = Arc::clone(&p2p_signal_cell);

    let close_config = config.clone();
    let close_target_token_id = target_token_id;
    let close_client_cell = Arc::clone(&client_cell);
    let close_duplex = duplex;
    let close_attach_state = Arc::clone(&attach_state);
    let close_detached = Arc::clone(&detached);
    let close_reconnecting = Arc::clone(&reconnecting);
    let close_p2p_signal_cell = Arc::clone(&p2p_signal_cell);
    let close_viewer_session_id = viewer_session_id.clone();
    let close_last_ack_seq = Arc::clone(&last_ack_seq);
    let close_has_attached_before = Arc::clone(&has_attached_before);
    let close_suspended = Arc::clone(&suspended);

    let client = ClientBuilder::new(config.server_url.clone())
        .transport_type(TransportType::Websocket)
        .auth(auth)
        .on("open", move |_payload, socket: RawClient| {
            *open_client_cell
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(socket.clone());
            let attach_state = Arc::clone(&open_attach_state);
            let target_token_id = open_target_token_id.clone();
            let viewer_session_id = open_viewer_session_id.clone();
            let last_ack_seq = open_last_ack_seq.load(Ordering::SeqCst);
            let has_attached_before = open_has_attached_before.load(Ordering::SeqCst);
            // Routed through a dedicated thread rather than
            // `spawn_socket_emit` because, unlike the fire-and-forget
            // signaling emits elsewhere in this file, a failure here must
            // be reported back through `signal_attach_result` so
            // `wait_for_attach_result` doesn't hang forever.
            std::thread::spawn(move || {
                let emit_result = if has_attached_before {
                    socket.emit(
                        "term:reattach",
                        json!({
                            "target_token_id": target_token_id,
                            "viewer_session_id": viewer_session_id,
                            "last_seq": last_ack_seq,
                        }),
                    )
                } else {
                    socket.emit(
                        "term:attach",
                        json!({
                            "target_token_id": target_token_id,
                            "viewer_session_id": viewer_session_id,
                        }),
                    )
                };
                if let Err(err) = emit_result {
                    signal_attach_result(
                        &attach_state,
                        Err(format!("failed to send term:attach/term:reattach: {err}")),
                    );
                }
            });
        })
        .on("term:reattach_ack", move |payload, socket: RawClient| {
            let Some(value) = payload_as_value(&payload) else {
                signal_attach_result(
                    &reattach_ack_attach_state,
                    Err("malformed term:reattach_ack".to_owned()),
                );
                return;
            };
            let Some(resume_seq) = value.get("resume_seq").and_then(|v| v.as_u64()) else {
                signal_attach_result(
                    &reattach_ack_attach_state,
                    Err("term:reattach_ack missing resume_seq".to_owned()),
                );
                return;
            };
            if let Some(replay_bytes) = value.get("replay_bytes").and_then(decode_bytes_field) {
                if !replay_bytes.is_empty() {
                    reattach_ack_duplex.push(&replay_bytes);
                }
            }
            reattach_ack_last_ack_seq.store(resume_seq, Ordering::SeqCst);
            reattach_ack_has_attached_before.store(true, Ordering::SeqCst);
            signal_attach_result(&reattach_ack_attach_state, Ok(()));
            spawn_viewer_p2p_negotiation(
                socket,
                reattach_ack_duplex.clone(),
                Arc::clone(&reattach_ack_p2p_signal_cell),
            );
        })
        .on("term:reattach_error", move |payload, socket: RawClient| {
            let reason =
                extract_str_field(&payload, "reason").unwrap_or_else(|| "unknown".to_owned());
            debug!(reason = %reason, "cloud viewer: reattach rejected, falling back to fresh attach");
            // Fresh session unknown to (or outrun on) the host -- fall back
            // to a plain attach on this same, already-open socket rather
            // than erroring the whole connect attempt out. Accepts the
            // resulting gap in output (same as today's pre-Задача-10
            // behavior) rather than hanging or retrying a reattach that
            // will keep failing the same way.
            let attach_state = Arc::clone(&reattach_error_attach_state);
            let target_token_id = reattach_error_target_token_id.clone();
            let viewer_session_id = reattach_error_viewer_session_id.clone();
            // Same reasoning as the `open` handler above: needs to report
            // a failed emit back via `signal_attach_result`, so this can't
            // just use the fire-and-forget `spawn_socket_emit`.
            std::thread::spawn(move || {
                if let Err(err) = socket.emit(
                    "term:attach",
                    json!({
                        "target_token_id": target_token_id,
                        "viewer_session_id": viewer_session_id,
                    }),
                ) {
                    signal_attach_result(
                        &attach_state,
                        Err(format!("failed to send fallback term:attach: {err}")),
                    );
                }
            });
        })
        .on("term:attach:ack", move |_payload, socket: RawClient| {
            ack_has_attached_before.store(true, Ordering::SeqCst);
            ack_last_ack_seq.store(0, Ordering::SeqCst);
            signal_attach_result(&ack_attach_state, Ok(()));
            spawn_viewer_p2p_negotiation(socket, ack_duplex.clone(), Arc::clone(&ack_p2p_signal_cell));
        })
        .on("term:attach:error", move |payload, _socket| {
            let reason =
                extract_str_field(&payload, "reason").unwrap_or_else(|| "unknown".to_owned());
            signal_attach_result(&error_attach_state, Err(reason));
        })
        .on("term:frame", move |payload, _socket| {
            if let Some(bytes) = extract_bare_bytes(&payload) {
                if frame_suspended.swap(false, Ordering::SeqCst) {
                    debug!("cloud viewer: host resumed sending frames, clearing suspended state");
                }
                frame_last_ack_seq.fetch_add(bytes.len() as u64, Ordering::SeqCst);
                frame_duplex.push(&bytes);
            } else {
                debug!("cloud viewer: term:frame with unparseable payload");
            }
        })
        .on("term:suspended", move |payload, _socket| {
            // The HOST's own connection to the relay dropped (Задача 2
            // grace period) -- not this viewer's. Unlike `term:detached`,
            // the duplex stays open: reads just keep blocking as if the
            // host were momentarily slow, so the local terminal never sees
            // an EOF for what may resolve itself within the grace window.
            suspended_flag.store(true, Ordering::SeqCst);
            warn!(
                payload = ?payload,
                "cloud viewer: host suspended (reconnecting on its end), waiting for it to resume"
            );
        })
        .on("term:webrtc_answer", move |payload, _socket| {
            let Some(value) = payload_as_value(&payload) else {
                return;
            };
            let Some(sdp) = value.get("sdp").and_then(|v| v.as_str()) else {
                return;
            };
            let guard = answer_p2p_signal_cell
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(RemoteSignal::Answer(sdp.to_owned()));
            }
        })
        .on("term:webrtc_ice", move |payload, _socket| {
            let Some(value) = payload_as_value(&payload) else {
                return;
            };
            let Some(candidate) = value.get("candidate") else {
                return;
            };
            let Ok(candidate_json) = serde_json::to_string(candidate) else {
                return;
            };
            let guard = ice_p2p_signal_cell
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(RemoteSignal::IceCandidate(candidate_json));
            }
        })
        .on("term:detached", move |_payload, _socket| {
            // Host ended the session on purpose -- mark it so the "close"
            // event that follows this one does not try to reconnect, then
            // signal EOF to the local terminal exactly as before.
            detach_detached.store(true, Ordering::SeqCst);
            detach_suspended.store(false, Ordering::SeqCst);
            detach_duplex.close();
        })
        .on("close", move |payload, _socket| {
            if close_detached.load(Ordering::SeqCst) {
                return;
            }
            if close_reconnecting.swap(true, Ordering::SeqCst) {
                // A reconnect loop from a previous "close" is already running.
                return;
            }
            warn!(
                payload = ?payload,
                "cloud viewer: connection closed unexpectedly, reconnecting"
            );
            let config = close_config.clone();
            let target_token_id = close_target_token_id.clone();
            let client_cell = Arc::clone(&close_client_cell);
            let duplex = close_duplex.clone();
            let attach_state = Arc::clone(&close_attach_state);
            let detached = Arc::clone(&close_detached);
            let reconnecting = Arc::clone(&close_reconnecting);
            let p2p_signal_cell = Arc::clone(&close_p2p_signal_cell);
            let viewer_session_id = close_viewer_session_id.clone();
            let last_ack_seq = Arc::clone(&close_last_ack_seq);
            let has_attached_before = Arc::clone(&close_has_attached_before);
            let suspended = Arc::clone(&close_suspended);
            std::thread::spawn(move || {
                viewer_reconnect_loop(
                    config,
                    target_token_id,
                    client_cell,
                    duplex,
                    attach_state,
                    detached,
                    suspended,
                    reconnecting,
                    p2p_signal_cell,
                    viewer_session_id,
                    last_ack_seq,
                    has_attached_before,
                );
            });
        })
        .on("error", |payload, _socket| {
            warn!(payload = ?payload, "cloud viewer: socket.io error");
        })
        .connect()
        .map_err(|err| {
            io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("cloud viewer: connect failed: {err}"),
            )
        })?;

    Ok(client)
}

/// Runs after an unexpected `"close"` (anything other than a graceful
/// `term:detached`), retrying `connect_viewer_attempt` with exponential
/// backoff until either a fresh connection re-attaches successfully or the
/// duplex is closed from elsewhere (e.g. a `term:detached` that arrives
/// while a retry is in flight). The local terminal never sees an EOF for a
/// transient network blip -- reads just block a little longer while this
/// loop is working, same as `CloudDuplex::read`'s existing wait behaviour.
#[allow(clippy::too_many_arguments)]
fn viewer_reconnect_loop(
    config: LegionConfig,
    target_token_id: String,
    client_cell: Arc<Mutex<Option<RawClient>>>,
    duplex: CloudDuplex,
    attach_state: Arc<(Mutex<Option<AttachResult>>, Condvar)>,
    detached: Arc<AtomicBool>,
    suspended: Arc<AtomicBool>,
    reconnecting: Arc<AtomicBool>,
    p2p_signal_cell: Arc<Mutex<Option<std::sync::mpsc::Sender<RemoteSignal>>>>,
    viewer_session_id: String,
    last_ack_seq: Arc<std::sync::atomic::AtomicU64>,
    has_attached_before: Arc<AtomicBool>,
) {
    let mut delay = VIEWER_RECONNECT_INITIAL_DELAY;
    loop {
        if detached.load(Ordering::SeqCst) {
            reconnecting.store(false, Ordering::SeqCst);
            return;
        }
        std::thread::sleep(delay);

        let attempt = connect_viewer_attempt(
            &config,
            target_token_id.clone(),
            Arc::clone(&client_cell),
            duplex.clone(),
            Arc::clone(&attach_state),
            Arc::clone(&detached),
            Arc::clone(&suspended),
            Arc::clone(&reconnecting),
            Arc::clone(&p2p_signal_cell),
            viewer_session_id.clone(),
            Arc::clone(&last_ack_seq),
            Arc::clone(&has_attached_before),
        )
        .and_then(|client| {
            drop(client);
            wait_for_attach_result(&attach_state)
        });

        match attempt {
            Ok(()) => {
                warn!("cloud viewer: reconnected after unexpected close");
                reconnecting.store(false, Ordering::SeqCst);
                return;
            }
            Err(err) => {
                warn!(
                    err = %err,
                    delay = ?delay,
                    "cloud viewer: reconnect attempt failed, retrying"
                );
                delay = (delay * 2).min(VIEWER_RECONNECT_MAX_DELAY);
            }
        }
    }
}

fn signal_attach_result(state: &Arc<(Mutex<Option<AttachResult>>, Condvar)>, result: AttachResult) {
    let (lock, cvar) = &**state;
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        *guard = Some(result);
        cvar.notify_all();
    }
}

fn wait_for_attach_result(state: &Arc<(Mutex<Option<AttachResult>>, Condvar)>) -> io::Result<()> {
    let (lock, cvar) = &**state;
    let mut guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let deadline = std::time::Instant::now() + ATTACH_TIMEOUT;

    loop {
        if let Some(result) = guard.take() {
            return result.map_err(|reason| {
                io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("cloud viewer: attach rejected: {reason}"),
                )
            });
        }

        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "cloud viewer: timed out waiting for term:attach:ack/term:attach:error",
            ));
        }

        let (next_guard, wait_result) = cvar
            .wait_timeout(guard, deadline - now)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard = next_guard;
        if wait_result.timed_out() && guard.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "cloud viewer: timed out waiting for term:attach:ack/term:attach:error",
            ));
        }
    }
}
