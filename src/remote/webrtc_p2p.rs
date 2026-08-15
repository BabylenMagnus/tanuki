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
//! BUILD-VERIFIED (2026-08-15, `cargo check`/`cargo clippy` clean against the
//! prebuilt `libghostty-vt`) but NOT yet exercised end-to-end against a real
//! host/viewer pair over an actual network. This module was originally
//! written by reading `webrtc-rs`'s own `examples/data-channels-offer-answer`
//! pattern from memory; that first draft had a real, since-fixed bug (see
//! `spawn_negotiation`'s doc comment: the negotiation thread's own Tokio
//! runtime was dropped, and so forcibly cancelled the just-opened data
//! channel's serving tasks, within microseconds of a successful negotiation
//! -- every P2P upgrade was self-destructing on success). Treat live P2P
//! sessions as still worth watching closely (via `term:webrtc_stats`
//! telemetry / `warn!` logs) until this has actually been run against a real
//! host/viewer pair across two machines.
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
            // Only the handshake (up to the data channel opening) is
            // timeout-bounded. Once open, `established` below carries the
            // peer connection and the signaling task's `JoinHandle` back
            // out *unconsumed* -- awaiting the handle after this timeout
            // (see below) is what keeps this thread's runtime alive for as
            // long as the caller holds `remote_tx`, instead of tearing it
            // (and every webrtc-rs background task riding on it: the mux
            // demuxer, the SCTP association, the data channel reader, and
            // `make_write_fn`'s own forwarder) down the instant the channel
            // opens. See this module's doc comment and `spawn_negotiation`'s
            // caller-facing doc for the full story.
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
            let established = matches!(outcome, Ok(Ok(_)));
            match &outcome {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => {
                    debug!(err = %err, "p2p: negotiation failed, staying on relay fallback");
                }
                Err(_) => {
                    debug!("p2p: negotiation timed out, staying on relay fallback");
                }
            }
            let best_local_candidate_type = best_candidate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .map(|(_, name)| name);
            (on_outcome)(NegotiationOutcome {
                established,
                best_local_candidate_type,
            });

            // Not timeout-bounded: `signaling_handle` only resolves once
            // `remote_rx` closes, which happens when the caller drops
            // `remote_tx` -- already exactly how `negotiations`/
            // `p2p_signal_cell` end a P2P leg's life in `cloud.rs` (viewer
            // detach on the host side, reconnect/replace on the viewer
            // side). Blocking on it here, inside the still-alive `rt`, is
            // what keeps `peer_connection` (and its background tasks) from
            // being cancelled while the data channel is still in use.
            if let Ok(Ok((peer_connection, signaling_handle))) = outcome {
                let _ = signaling_handle.await;
                drop(peer_connection);
            }
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
) -> Result<(Arc<webrtc::peer_connection::RTCPeerConnection>, tokio::task::JoinHandle<()>), String> {
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

    // Runs concurrently with waiting for `ready_rx` below (answer/ICE
    // application must keep happening while the channel is still opening),
    // and keeps running after: it only ends once `remote_rx` closes, i.e.
    // once the caller drops `remote_tx`. `spawn_negotiation` awaits this
    // handle *after* the handshake timeout window (see there), which is
    // what keeps this negotiation's dedicated runtime -- and therefore
    // `peer_connection` and every webrtc-rs task riding on it -- alive for
    // the life of the P2P leg instead of being torn down the instant the
    // channel opens.
    let signaling_handle = tokio::spawn(signaling_loop);

    let data_channel = ready_rx.await.map_err(|_| {
        signaling_handle.abort();
        "data channel never became ready".to_owned()
    })?;
    let write_fn = make_write_fn(data_channel);
    (on_channel_ready)(write_fn);
    Ok((peer_connection, signaling_handle))
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
