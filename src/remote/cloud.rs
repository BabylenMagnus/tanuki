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
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use base64::Engine as _;
use rust_socketio::{ClientBuilder, Payload, RawClient, TransportType};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::ipc::Transport;
use crate::server::client_transport::{self, ServerEvent};

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

// ---------------------------------------------------------------------------
// Legion device identity (`~/.legion/config.json`)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
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

struct CloudDuplexState {
    buffer: std::collections::VecDeque<u8>,
    closed: bool,
}

struct CloudDuplexInner {
    state: Mutex<CloudDuplexState>,
    ready: Condvar,
    write_fn: Arc<WriteFn>,
    /// Sticky read timeout set via [`CloudDuplex::set_recv_timeout`],
    /// mirroring `SO_RCVTIMEO` semantics on a real socket: `None` blocks
    /// forever, `Some(d)` bounds every subsequent `read()` call until
    /// changed again. Without this, a stuck relay (e.g. a `term:frame`
    /// that never arrives because the server routed `term:peer_attached`
    /// to a dead host sid) hangs the client forever with no error --
    /// exactly the failure mode this exists to turn into a clear
    /// `TimedOut` instead.
    read_timeout: Mutex<Option<Duration>>,
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
        Self {
            inner: Arc::new(CloudDuplexInner {
                state: Mutex::new(CloudDuplexState {
                    buffer: std::collections::VecDeque::new(),
                    closed: false,
                }),
                ready: Condvar::new(),
                write_fn,
                read_timeout: Mutex::new(None),
            }),
        }
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
        (self.inner.write_fn)(buf)?;
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
/// Kept alive for as long as cloud hosting should stay active; dropping it
/// does not currently tear down the underlying connection (see the final
/// report's note on lifecycle -- shutdown wiring is left to the CLI-flag
/// follow-up task).
pub(crate) struct CloudHostTransport {
    #[allow(dead_code)]
    client: rust_socketio::client::Client,
    viewers: Arc<Mutex<HashMap<String, CloudDuplex>>>,
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
        std::thread::spawn(move || {
            let _ = result_tx.send(Self::connect(
                client_id_allocator,
                server_event_tx,
                should_quit,
            ));
        });
        result_rx.recv().map_err(|_| {
            io::Error::other("cloud host: connect thread terminated without a result")
        })?
    }

    fn connect(
        client_id_allocator: impl Fn() -> u64 + Send + Sync + 'static,
        server_event_tx: mpsc::Sender<ServerEvent>,
        should_quit: Arc<std::sync::atomic::AtomicBool>,
    ) -> io::Result<Self> {
        let config = load_legion_config()?;
        let auth = auth_payload(&config, "term-host");

        let viewers: Arc<Mutex<HashMap<String, CloudDuplex>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let client_id_allocator = Arc::new(client_id_allocator);

        let attach_viewers = Arc::clone(&viewers);
        let attach_client_id_allocator = Arc::clone(&client_id_allocator);
        let attach_server_event_tx = server_event_tx.clone();
        let attach_should_quit = Arc::clone(&should_quit);

        let detach_viewers = Arc::clone(&viewers);
        let input_viewers = Arc::clone(&viewers);

        // Populated from the "open" callback and reused by the heartbeat
        // thread and the host_hello:error retry to re-emit term:host_hello
        // without needing a fresh RawClient handle each time.
        let host_client_cell: Arc<Mutex<Option<RawClient>>> = Arc::new(Mutex::new(None));
        let open_host_client_cell = Arc::clone(&host_client_cell);
        let retry_host_client_cell = Arc::clone(&host_client_cell);
        let heartbeat_host_client_cell = Arc::clone(&host_client_cell);

        let client = ClientBuilder::new(config.server_url.clone())
            .transport_type(TransportType::Websocket)
            .auth(auth)
            .on("open", move |_payload, socket: RawClient| {
                *open_host_client_cell
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(socket.clone());
                if let Err(err) = socket.emit("term:host_hello", json!({})) {
                    warn!(err = %err, "cloud host: failed to send term:host_hello");
                }
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
                let duplex = CloudDuplex::new(detach_write_fn(write_fn));

                attach_viewers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(viewer_sid.clone(), duplex.clone());

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
                if let Some(duplex) = detach_viewers
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&viewer_sid)
                {
                    duplex.close();
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
            .connect()
            .map_err(|err| {
                io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("cloud host: connect failed: {err}"),
                )
            })?;

        // Periodically re-send term:host_hello so the backend's TTL'd
        // registration (`term:host_sid:{token}`, see term_relay.py) never
        // expires while this host is genuinely still alive. Runs for the
        // lifetime of the process, same lifecycle as the client/viewer
        // connections themselves.
        std::thread::spawn(move || loop {
            std::thread::sleep(HOST_HEARTBEAT_INTERVAL);
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

        Ok(Self { client, viewers })
    }

    /// Number of viewers currently attached to this host session.
    #[allow(dead_code)]
    pub(crate) fn attached_viewer_count(&self) -> usize {
        self.viewers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

// ---------------------------------------------------------------------------
// Viewer side: connect_viewer
// ---------------------------------------------------------------------------

type AttachResult = Result<(), String>;

/// Connects to the Tanuki backend as a tanuki cloud viewer attaching to
/// `target_token_id` (another of the caller's own paired devices), and
/// blocks until the attach either succeeds or is rejected.
///
/// On success, returns a [`Transport::Cloud`] that behaves like any other
/// `Transport` -- callers use it exactly where they would otherwise use
/// `Transport::Local(crate::ipc::connect_local_stream(...)?)`.
pub(crate) fn connect_viewer(target_token_id: &str) -> io::Result<Transport> {
    let config = load_legion_config()?;
    let auth = auth_payload(&config, "term-viewer");
    let target_token_id = target_token_id.to_owned();

    // The Socket.IO client handle needed to emit `term:input` at write-time
    // (from arbitrary caller threads, outside any event callback) is only
    // available once `.connect()` returns below. Every `on(...)` callback
    // receives its own `RawClient` handle immediately though, so we grab a
    // copy of it from the very first callback that fires ("open") and stash
    // it here for the write closure to use afterwards.
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
    let duplex = CloudDuplex::new(detach_write_fn(write_fn));

    let attach_state: Arc<(Mutex<Option<AttachResult>>, Condvar)> =
        Arc::new((Mutex::new(None), Condvar::new()));

    let open_client_cell = Arc::clone(&client_cell);
    let open_attach_state = Arc::clone(&attach_state);
    let ack_attach_state = Arc::clone(&attach_state);
    let error_attach_state = Arc::clone(&attach_state);
    let frame_duplex = duplex.clone();
    let detach_duplex = duplex.clone();

    let client = ClientBuilder::new(config.server_url.clone())
        .transport_type(TransportType::Websocket)
        .auth(auth)
        .on("open", move |_payload, socket: RawClient| {
            *open_client_cell
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(socket.clone());
            if let Err(err) =
                socket.emit("term:attach", json!({ "target_token_id": target_token_id }))
            {
                signal_attach_result(
                    &open_attach_state,
                    Err(format!("failed to send term:attach: {err}")),
                );
            }
        })
        .on("term:attach:ack", move |_payload, _socket| {
            signal_attach_result(&ack_attach_state, Ok(()));
        })
        .on("term:attach:error", move |payload, _socket| {
            let reason =
                extract_str_field(&payload, "reason").unwrap_or_else(|| "unknown".to_owned());
            signal_attach_result(&error_attach_state, Err(reason));
        })
        .on("term:frame", move |payload, _socket| {
            if let Some(bytes) = extract_bare_bytes(&payload) {
                frame_duplex.push(&bytes);
            } else {
                debug!("cloud viewer: term:frame with unparseable payload");
            }
        })
        .on("term:detached", move |_payload, _socket| {
            detach_duplex.close();
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

    // The connection is kept alive by the crate's own background poll
    // thread (which holds its own client reference), not by this local
    // binding, so it is fine to let `client` go out of scope once we are
    // done using it here.
    drop(client);

    wait_for_attach_result(&attach_state)?;

    Ok(Transport::Cloud(duplex))
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
