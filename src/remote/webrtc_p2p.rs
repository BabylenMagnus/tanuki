//! P2P data-channel negotiation for the `--cloud` transport (Task 5,
//! `spelflow-tanuki-terminal-stability-tasks.md`).
//!
//! `tanuki_api`/`term_relay.py` only relays SDP/ICE now (`term:webrtc_offer`,
//! `term:webrtc_answer`, `term:webrtc_ice` -- see `term_relay.py`); the
//! actual terminal byte stream is negotiated directly between host and
//! viewer here, via `webrtc-rs`. If negotiation fails or does not complete
//! within `timeout`, the caller's existing Socket.IO byte-relay
//! (`CloudDuplex`'s original write path, `cloud.rs`) is left completely
//! untouched -- this module never tears down or blocks that fallback.
//!
//! NOT BUILD-VERIFIED. This environment has no working `tanuki_terminal`
//! toolchain (no `zig` for `build.rs`'s vendored `libghostty-vt` step -- see
//! `wiki/tanuki-terminal-stability-implementation.md`, "Verification
//! status"). This module was written by reading `webrtc-rs`'s own
//! `examples/data-channels-offer-answer` pattern from memory and has never
//! been compiled. Treat it as a reviewed draft, not a proven
//! implementation, until it has actually built and been exercised against
//! a real host/viewer pair.
//!
//! ## Design
//!
//! Both sides run this on a dedicated OS thread with its own fresh Tokio
//! runtime (same "nested runtime" precaution used elsewhere in
//! `remote::cloud` -- callers here run inside `rust_socketio`'s own
//! `block_on`, see `detach_write_fn`'s doc comment). The caller is
//! responsible for:
//!
//! - wiring incoming `term:webrtc_offer` / `term:webrtc_answer` /
//!   `term:webrtc_ice` Socket.IO events into the [`RemoteSignal`] sender
//!   returned by [`spawn_negotiation`];
//! - supplying `emit_offer` / `emit_answer` / `emit_ice` closures that push
//!   this side's local SDP/ICE out over that same Socket.IO channel;
//! - supplying `on_channel_ready`, invoked at most once, with a write
//!   function that sends bytes over the now-open data channel (wire this
//!   into `CloudDuplex::set_write_fn`);
//! - supplying `on_data`, invoked for every inbound data-channel message
//!   (wire this into `CloudDuplex::push`).

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use tracing::{debug, warn};

use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

/// A signaling message received over the existing Socket.IO channel and
/// forwarded into an in-progress negotiation.
pub(crate) enum RemoteSignal {
    /// The offerer's SDP offer (answerer side only).
    Offer(String),
    /// The answerer's SDP answer (offerer side only).
    Answer(String),
    /// One trickled ICE candidate, as the JSON body of `RTCIceCandidateInit`.
    IceCandidate(String),
}

type WriteFn = dyn Fn(&[u8]) -> io::Result<()> + Send + Sync;

/// Callback invoked for every inbound data-channel message.
pub(crate) type DataCallback = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Reported once per negotiation attempt (Task 7,
/// `spelflow-tanuki-terminal-stability-tasks.md`) so the caller can relay
/// it to the backend as telemetry (`term:webrtc_stats`, see `cloud.rs`).
///
/// `best_local_candidate_type` is an approximation, not a precise "which
/// candidate pair actually got selected" report: it is the most-direct
/// type (`host` > `srflx`/`prflx` > `relay`) seen among the candidates
/// *this* side gathered during ICE, parsed from each candidate's `typ`
/// token. Getting the exact selected pair requires
/// `RTCPeerConnection::get_stats()`, which this draft avoids relying on
/// (higher API-shape risk in a never-compiled module, see this file's
/// top-level doc comment) -- swap to precise stats once this module has
/// actually built and run once.
pub(crate) struct NegotiationOutcome {
    pub(crate) established: bool,
    pub(crate) best_local_candidate_type: Option<&'static str>,
}

/// Ranks candidate types from most to least direct; lower is better. Used
/// to track the single "best" type gathered across every `on_ice_candidate`
/// callback firing during one negotiation.
fn candidate_type_rank(typ: &str) -> Option<(u8, &'static str)> {
    match typ {
        "host" => Some((0, "host")),
        "srflx" => Some((1, "srflx")),
        "prflx" => Some((2, "prflx")),
        "relay" => Some((3, "relay")),
        _ => None,
    }
}

/// Extracts the `typ <word>` token from a raw ICE candidate SDP line, e.g.
/// `"candidate:1 1 udp 2122260223 10.0.0.5 54321 typ host"` -> `"host"`.
fn parse_candidate_type(candidate_sdp: &str) -> Option<&str> {
    let mut tokens = candidate_sdp.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "typ" {
            return tokens.next();
        }
    }
    None
}

/// Starts P2P negotiation on a dedicated thread and returns immediately.
///
/// `is_offerer` must be `true` on exactly one side of a given host/viewer
/// pair -- the viewer is the offerer (it initiates `term:attach`), the host
/// is the answerer, matching the direction `term_relay.py` already routes
/// `term:webrtc_offer`/`term:webrtc_answer` in.
///
/// Returns a channel the caller feeds incoming `term:webrtc_*` events into.
/// Dropping the returned sender (or letting it go out of scope once the
/// underlying Socket.IO connection is torn down) simply lets the
/// negotiation task observe channel closure and exit -- no explicit cancel
/// call is needed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_negotiation(
    is_offerer: bool,
    ice_servers: Vec<String>,
    emit_offer: Arc<dyn Fn(&str) + Send + Sync>,
    emit_answer: Arc<dyn Fn(&str) + Send + Sync>,
    emit_ice: Arc<dyn Fn(&str) + Send + Sync>,
    on_channel_ready: Arc<dyn Fn(Arc<WriteFn>) + Send + Sync>,
    on_data: DataCallback,
    on_outcome: Arc<dyn Fn(NegotiationOutcome) + Send + Sync>,
    timeout: Duration,
) -> std::sync::mpsc::Sender<RemoteSignal> {
    let (remote_tx, remote_rx) = std::sync::mpsc::channel::<RemoteSignal>();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(err) => {
                warn!(err = %err, "p2p: failed to build negotiation runtime");
                return;
            }
        };

        // Bridge the sync std::sync::mpsc receiver into the async world: a
        // plain forwarding loop on a blocking thread, since std::mpsc has
        // no async recv. Ends when `remote_tx` is dropped by the caller.
        let (async_remote_tx, async_remote_rx) = tokio_mpsc::unbounded_channel::<RemoteSignal>();
        std::thread::spawn(move || {
            for signal in remote_rx {
                if async_remote_tx.send(signal).is_err() {
                    break;
                }
            }
        });

        // Shared across every `on_ice_candidate` firing for this
        // negotiation (Task 7 telemetry) -- holds `(rank, name)` for the
        // most-direct candidate type gathered so far.
        let best_candidate: Arc<Mutex<Option<(u8, &'static str)>>> = Arc::new(Mutex::new(None));

        rt.block_on(async move {
            let outcome = tokio::time::timeout(
                timeout,
                run_negotiation(
                    is_offerer,
                    ice_servers,
                    emit_offer,
                    emit_answer,
                    emit_ice,
                    async_remote_rx,
                    on_channel_ready,
                    on_data,
                    Arc::clone(&best_candidate),
                ),
            )
            .await;
            let established = match &outcome {
                Ok(Ok(())) => true,
                Ok(Err(err)) => {
                    debug!(err = %err, "p2p: negotiation failed, staying on relay fallback");
                    false
                }
                Err(_) => {
                    debug!("p2p: negotiation timed out, staying on relay fallback");
                    false
                }
            };
            let best_local_candidate_type = best_candidate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .map(|(_, name)| name);
            (on_outcome)(NegotiationOutcome {
                established,
                best_local_candidate_type,
            });
        });
    });

    remote_tx
}

fn default_ice_servers(extra: Vec<String>) -> Vec<RTCIceServer> {
    let mut urls = vec![
        "stun:stun.l.google.com:19302".to_owned(),
        "stun:stun1.l.google.com:19302".to_owned(),
    ];
    urls.extend(extra);
    vec![RTCIceServer {
        urls,
        ..Default::default()
    }]
}

#[allow(clippy::too_many_arguments)]
async fn run_negotiation(
    is_offerer: bool,
    ice_servers: Vec<String>,
    emit_offer: Arc<dyn Fn(&str) + Send + Sync>,
    emit_answer: Arc<dyn Fn(&str) + Send + Sync>,
    emit_ice: Arc<dyn Fn(&str) + Send + Sync>,
    mut remote_rx: tokio_mpsc::UnboundedReceiver<RemoteSignal>,
    on_channel_ready: Arc<dyn Fn(Arc<WriteFn>) + Send + Sync>,
    on_data: DataCallback,
    best_candidate: Arc<Mutex<Option<(u8, &'static str)>>>,
) -> Result<(), String> {
    let mut media_engine = MediaEngine::default();
    // No audio/video codecs registered -- data-channel-only usage still
    // requires a MediaEngine per webrtc-rs's API, left at defaults.
    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut media_engine)
        .map_err(|err| format!("interceptor registry: {err}"))?;

    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build();

    let config = RTCConfiguration {
        ice_servers: default_ice_servers(ice_servers),
        ..Default::default()
    };

    let peer_connection = Arc::new(
        api.new_peer_connection(config)
            .await
            .map_err(|err| format!("new_peer_connection: {err}"))?,
    );

    // Trickle ICE: forward every locally-gathered candidate out over
    // signaling as soon as it's found, and track the most-direct type seen
    // so far (Task 7 telemetry, see `NegotiationOutcome`).
    let ice_emit = Arc::clone(&emit_ice);
    let ice_best_candidate = Arc::clone(&best_candidate);
    peer_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
        let ice_emit = Arc::clone(&ice_emit);
        let best_candidate = Arc::clone(&ice_best_candidate);
        Box::pin(async move {
            let Some(candidate) = candidate else {
                return;
            };
            match candidate.to_json() {
                Ok(init) => {
                    if let Some(typ) = parse_candidate_type(&init.candidate) {
                        if let Some(ranked) = candidate_type_rank(typ) {
                            let mut best = best_candidate
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            let should_replace =
                                best.map(|(rank, _)| ranked.0 < rank).unwrap_or(true);
                            if should_replace {
                                *best = Some(ranked);
                            }
                        }
                    }
                    match serde_json::to_string(&init) {
                        Ok(json) => (ice_emit)(&json),
                        Err(err) => warn!(err = %err, "p2p: failed to serialize ICE candidate"),
                    }
                }
                Err(err) => warn!(err = %err, "p2p: failed to convert ICE candidate to JSON"),
            }
        }) as Pin<Box<dyn Future<Output = ()> + Send>>
    }));

    // Fires once the data channel opens on either side (offerer's own
    // created channel, or the answerer's channel handed to it via
    // on_data_channel below).
    let (ready_tx, ready_rx) = oneshot::channel::<Arc<RTCDataChannel>>();

    if is_offerer {
        let data_channel = peer_connection
            .create_data_channel(
                "term",
                Some(RTCDataChannelInit {
                    ordered: Some(true),
                    // No retransmit limit -- terminal output must not drop
                    // or reorder bytes (matches Task 5's Acceptance
                    // Criteria).
                    max_retransmits: None,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|err| format!("create_data_channel: {err}"))?;

        wire_data_channel(Arc::clone(&data_channel), on_data, ready_tx);

        let offer = peer_connection
            .create_offer(None)
            .await
            .map_err(|err| format!("create_offer: {err}"))?;
        peer_connection
            .set_local_description(offer.clone())
            .await
            .map_err(|err| format!("set_local_description(offer): {err}"))?;
        (emit_offer)(&offer.sdp);
    } else {
        // Answerer: the remote offerer creates the data channel, we just
        // need to catch it here.
        let mut ready_tx = Some(ready_tx);
        let on_data = Arc::clone(&on_data);
        peer_connection.on_data_channel(Box::new(move |data_channel: Arc<RTCDataChannel>| {
            if let Some(ready_tx) = ready_tx.take() {
                wire_data_channel(data_channel, Arc::clone(&on_data), ready_tx);
            }
            Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>
        }));

        // Wait for the offer before answering.
        let offer_sdp = loop {
            match remote_rx
                .recv()
                .await
                .ok_or_else(|| "signaling channel closed before offer arrived".to_owned())?
            {
                RemoteSignal::Offer(sdp) => break sdp,
                RemoteSignal::IceCandidate(_) | RemoteSignal::Answer(_) => {
                    // Can arrive out of order relative to the offer in
                    // theory; ICE candidates before the offer can't be
                    // applied yet, so just wait for the offer first.
                    continue;
                }
            }
        };

        let remote_desc = RTCSessionDescription::offer(offer_sdp)
            .map_err(|err| format!("parse remote offer: {err}"))?;
        peer_connection
            .set_remote_description(remote_desc)
            .await
            .map_err(|err| format!("set_remote_description(offer): {err}"))?;

        let answer = peer_connection
            .create_answer(None)
            .await
            .map_err(|err| format!("create_answer: {err}"))?;
        peer_connection
            .set_local_description(answer.clone())
            .await
            .map_err(|err| format!("set_local_description(answer): {err}"))?;
        (emit_answer)(&answer.sdp);
    }

    // Drain remaining signaling messages (remote answer for the offerer,
    // and ICE candidates on both sides) concurrently with waiting for the
    // data channel to open.
    let signaling_pc = Arc::clone(&peer_connection);
    let signaling_loop = async move {
        while let Some(signal) = remote_rx.recv().await {
            match signal {
                RemoteSignal::Answer(sdp) => {
                    if !is_offerer {
                        continue;
                    }
                    match RTCSessionDescription::answer(sdp) {
                        Ok(desc) => {
                            if let Err(err) = signaling_pc.set_remote_description(desc).await {
                                warn!(err = %err, "p2p: failed to apply remote answer");
                            }
                        }
                        Err(err) => warn!(err = %err, "p2p: failed to parse remote answer"),
                    }
                }
                RemoteSignal::IceCandidate(candidate_json) => {
                    match serde_json::from_str::<RTCIceCandidateInit>(&candidate_json) {
                        Ok(init) => {
                            if let Err(err) = signaling_pc.add_ice_candidate(init).await {
                                warn!(err = %err, "p2p: failed to add remote ICE candidate");
                            }
                        }
                        Err(err) => warn!(err = %err, "p2p: failed to parse remote ICE candidate"),
                    }
                }
                RemoteSignal::Offer(_) => {
                    // Already consumed above on the answerer side; an
                    // offerer should never receive one.
                }
            }
        }
    };

    // Runs for the lifetime of the negotiation (answer/ICE application) --
    // spawned unconditionally rather than raced against `ready_rx` in a
    // `select!`, since a `select!` would need to both poll this future
    // *and* potentially move it into `tokio::spawn` from the other
    // branch's body, which the borrow checker rejects (a future can't be
    // polled and moved-out-of at the same time). The outer
    // `tokio::time::timeout` in `spawn_negotiation` still bounds how long
    // this whole function waits below.
    tokio::spawn(signaling_loop);

    let data_channel = ready_rx
        .await
        .map_err(|_| "data channel never became ready".to_owned())?;
    let write_fn = make_write_fn(data_channel);
    (on_channel_ready)(write_fn);
    // Keep `peer_connection` alive for the process lifetime by relying on
    // the Arc clone already captured inside `signaling_loop`'s spawned
    // task; this function's own local binding can be dropped safely.
    drop(peer_connection);
    Ok(())
}

fn wire_data_channel(
    data_channel: Arc<RTCDataChannel>,
    on_data: DataCallback,
    ready_tx: oneshot::Sender<Arc<RTCDataChannel>>,
) {
    let mut ready_tx = Some(ready_tx);
    let ready_data_channel = Arc::clone(&data_channel);
    data_channel.on_open(Box::new(move || {
        if let Some(ready_tx) = ready_tx.take() {
            let _ = ready_tx.send(Arc::clone(&ready_data_channel));
        }
        Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>
    }));

    data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
        (on_data)(msg.data.as_ref());
        Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>
    }));
}

/// Bridges the sync `write_fn` shape `CloudDuplex` expects onto the async
/// `RTCDataChannel::send`, via a background task on this negotiation's own
/// runtime that owns the channel and loops sending queued outbound bytes.
/// `tokio::sync::mpsc::UnboundedSender::send` is itself a plain sync call,
/// safe to invoke from any thread -- no nested-runtime issue here (unlike
/// `rust_socketio`'s `emit`, `RTCDataChannel::send` has no internal
/// `block_on`).
fn make_write_fn(data_channel: Arc<RTCDataChannel>) -> Arc<WriteFn> {
    let (tx, mut rx) = tokio_mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(chunk) = rx.recv().await {
            if let Err(err) = data_channel.send(&Bytes::from(chunk)).await {
                warn!(err = %err, "p2p: data channel send failed, dropping outbound data");
                break;
            }
        }
    });
    Arc::new(move |data: &[u8]| -> io::Result<()> {
        tx.send(data.to_vec())
            .map_err(|_| io::Error::other("p2p: data channel send task terminated"))
    })
}
