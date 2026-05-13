use std::collections::HashMap;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::runtime::Builder;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::ServeConfig;
use crate::webrtc::{
    HostSessionState, HostWebRtcEngine, PeerRole, PeerSummary, SignalDispatch, SignalMessage,
    SignalPayload,
};

const HOST_PEER_ID: &str = "desplio-host";

#[derive(Clone)]
pub struct PreviewPaths {
    pub video_path: PathBuf,
    pub latest_frame_path: PathBuf,
}

#[derive(Clone)]
struct ServerState {
    preview: PreviewPaths,
    peers: Arc<Mutex<HashMap<String, PeerHandle>>>,
    host_session: Arc<Mutex<HostSessionState>>,
    host_engine: Arc<Mutex<Option<HostWebRtcEngine>>>,
}

#[derive(Clone)]
struct PeerHandle {
    role: PeerRole,
    sender: mpsc::UnboundedSender<Message>,
}

pub fn spawn_preview_server(
    config: &ServeConfig,
    video_path: PathBuf,
    latest_frame_path: PathBuf,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>, io::Error> {
    let bind_addr = config.bind_addr.clone();
    let page_path = PathBuf::from(&config.page_path);
    let preview = PreviewPaths {
        video_path,
        latest_frame_path,
    };

    let handle = thread::Builder::new()
        .name("desplio-host-server".into())
        .spawn(move || {
            let runtime = match Builder::new_multi_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => {
                    warn!(error = %err, "failed to create async host server runtime");
                    return;
                }
            };

            runtime.block_on(async move {
                let socket_addr: SocketAddr = match bind_addr.parse() {
                    Ok(addr) => addr,
                    Err(err) => {
                        warn!(error = %err, bind_addr, "failed to parse host server bind address");
                        return;
                    }
                };

                let app_state = ServerState {
                    preview,
                    peers: Arc::new(Mutex::new(HashMap::new())),
                    host_session: Arc::new(Mutex::new(HostSessionState {
                        peer_id: HOST_PEER_ID.into(),
                        note: "Host signalling peer is ready for offer/ICE exchange".into(),
                        ..HostSessionState::default()
                    })),
                    host_engine: Arc::new(Mutex::new(None)),
                };

                let router = Router::new()
                    .route("/", get(serve_index))
                    .route("/latest.mp4", get(serve_latest_mp4))
                    .route("/video.mp4", get(serve_latest_mp4))
                    .route("/latest-frame.png", get(serve_latest_frame))
                    .route("/latest-frame.txt", get(serve_latest_frame_status))
                    .route("/status.txt", get(serve_status))
                    .route("/api/peers", get(list_peers))
                    .route("/api/host-session", get(host_session))
                    .route("/ws", get(ws_handler))
                    .with_state((page_path, app_state));

                let listener = match TcpListener::bind(socket_addr).await {
                    Ok(listener) => listener,
                    Err(err) => {
                        warn!(error = %err, bind_addr, "failed to bind host server");
                        return;
                    }
                };

                info!(bind_addr, "M3 host server is listening");

                let shutdown_signal = async move {
                    while !shutdown.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                };

                if let Err(err) = axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown_signal)
                    .await
                {
                    warn!(error = %err, "host server exited with error");
                }
            });
        })?;

    Ok(handle)
}

async fn serve_index(State((page_path, _state)): State<(PathBuf, ServerState)>) -> Response {
    match fs::read_to_string(&page_path) {
        Ok(contents) => with_no_store(Html(contents)).into_response(),
        Err(err) => internal_error(err),
    }
}

async fn serve_latest_mp4(State((_page_path, state)): State<(PathBuf, ServerState)>) -> Response {
    serve_bytes(&state.preview.video_path, "video/mp4")
}

async fn serve_latest_frame(State((_page_path, state)): State<(PathBuf, ServerState)>) -> Response {
    serve_bytes(&state.preview.latest_frame_path, "image/png")
}

async fn serve_latest_frame_status(
    State((_page_path, state)): State<(PathBuf, ServerState)>,
) -> Response {
    match fs::metadata(&state.preview.latest_frame_path) {
        Ok(meta) => {
            let body = format!(
                "latest_frame={}\nsize_bytes={}\n",
                state.preview.latest_frame_path.display(),
                meta.len(),
            );
            with_no_store(body).into_response()
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            not_found("stable preview frame has not been published yet")
        }
        Err(err) => internal_error(err),
    }
}

async fn serve_status(State((_page_path, state)): State<(PathBuf, ServerState)>) -> Response {
    let peers = state.peers.lock().await;
    let body = format!(
        "preview=ready\nlatest_segment={}\nlatest_frame={}\nconnected_peers={}\nhost_peer_id={}\n",
        state.preview.video_path.display(),
        state.preview.latest_frame_path.display(),
        peers.len(),
        HOST_PEER_ID,
    );
    with_no_store(body).into_response()
}

async fn list_peers(State((_page_path, state)): State<(PathBuf, ServerState)>) -> Response {
    let peers = state.peers.lock().await;
    let mut summaries: Vec<PeerSummary> = vec![PeerSummary {
        peer_id: HOST_PEER_ID.into(),
        role: PeerRole::Host,
    }];
    summaries.extend(peers
        .iter()
        .map(|(peer_id, peer)| PeerSummary {
            peer_id: peer_id.clone(),
            role: peer.role.clone(),
        })
        .collect::<Vec<_>>());
    with_no_store(Json(summaries)).into_response()
}

async fn host_session(State((_page_path, state)): State<(PathBuf, ServerState)>) -> Response {
    let session = state.host_session.lock().await.clone();
    with_no_store(Json(session)).into_response()
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State((_page_path, state)): State<(PathBuf, ServerState)>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: ServerState) {
    let (mut sender, mut receiver) = socket.split();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Message>();

    let send_task = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            if sender.send(message).await.is_err() {
                break;
            }
        }
    });

    let mut peer_id: Option<String> = None;

    while let Some(incoming) = receiver.next().await {
        let Ok(message) = incoming else {
            break;
        };

        match message {
            Message::Text(text) => {
                debug!(%text, "received signalling websocket message");
                match serde_json::from_str::<SignalMessage>(&text) {
                    Ok(SignalMessage::Hello {
                        peer_id: announced_peer_id,
                        role,
                        capabilities: _,
                    }) => {
                        peer_id = Some(announced_peer_id.clone());
                        state.peers.lock().await.insert(
                            announced_peer_id.clone(),
                            PeerHandle {
                                role: role.clone(),
                                sender: outbound_tx.clone(),
                            },
                        );

                        let peers_snapshot: Vec<PeerSummary> = state
                            .peers
                            .lock()
                            .await
                            .iter()
                            .map(|(peer_id, peer)| PeerSummary {
                                peer_id: peer_id.clone(),
                                role: peer.role.clone(),
                            })
                            .collect();
                        let mut peers_snapshot = peers_snapshot;
                        peers_snapshot.insert(
                            0,
                            PeerSummary {
                                peer_id: HOST_PEER_ID.into(),
                                role: PeerRole::Host,
                            },
                        );

                        let _ = outbound_tx.send(Message::Text(
                            serde_json::to_string(&SignalMessage::Peers {
                                peers: peers_snapshot,
                            })
                            .unwrap()
                            .into(),
                        ));

                        let host_session = state.host_session.lock().await.clone();
                        let _ = outbound_tx.send(Message::Text(
                            serde_json::to_string(&SignalMessage::HostSession {
                                session: host_session,
                            })
                            .unwrap()
                            .into(),
                        ));
                    }
                    Ok(SignalMessage::Signal { from, to, payload }) => {
                        if to == HOST_PEER_ID {
                            handle_host_signal(&state, &outbound_tx, &from, payload).await;
                            let session = state.host_session.lock().await.clone();
                            let _ = outbound_tx.send(Message::Text(
                                serde_json::to_string(&SignalMessage::HostSession { session })
                                    .unwrap()
                                    .into(),
                            ));
                            continue;
                        }

                        let target = {
                            let peers = state.peers.lock().await;
                            peers.get(&to).cloned()
                        };

                        if let Some(target) = target {
                            let relay = SignalMessage::Signal { from, to, payload };
                            let _ = target.sender.send(Message::Text(
                                serde_json::to_string(&relay).unwrap().into(),
                            ));
                        } else {
                            let error = SignalMessage::Error {
                                message: format!("target peer '{to}' is not connected"),
                            };
                            let _ = outbound_tx.send(Message::Text(
                                serde_json::to_string(&error).unwrap().into(),
                            ));
                        }
                    }
                    Ok(SignalMessage::Ping { nonce }) => {
                        let _ = outbound_tx.send(Message::Text(
                            serde_json::to_string(&SignalMessage::Pong { nonce })
                                .unwrap()
                                .into(),
                        ));
                    }
                    Ok(_) => {}
                    Err(err) => {
                        let error = SignalMessage::Error {
                            message: format!("invalid signalling message: {err}"),
                        };
                        let _ = outbound_tx.send(Message::Text(
                            serde_json::to_string(&error).unwrap().into(),
                        ));
                    }
                }
            }
            Message::Close(_) => break,
            Message::Ping(payload) => {
                let _ = outbound_tx.send(Message::Pong(payload));
            }
            _ => {}
        }
    }

    if let Some(peer_id) = peer_id {
        state.peers.lock().await.remove(&peer_id);
    }

    send_task.abort();
}

async fn update_host_session(state: &ServerState, from: &str, payload: &SignalPayload) {
    let mut session = state.host_session.lock().await;
    session.last_offer_from = Some(from.to_string());
    match payload {
        SignalPayload::Offer { .. } => {
            session.pending_offer = true;
            session.last_signal_kind = Some("offer".into());
            session.note = "Host received an SDP offer; media engine answer path is the next step.".into();
        }
        SignalPayload::Answer { .. } => {
            session.last_signal_kind = Some("answer".into());
            session.note = "Host received an SDP answer.".into();
        }
        SignalPayload::IceCandidate { .. } => {
            session.pending_ice_candidates += 1;
            session.last_signal_kind = Some("ice_candidate".into());
            session.note = "Host is collecting ICE candidates for the future WebRTC engine.".into();
        }
        SignalPayload::Renegotiate => {
            session.last_signal_kind = Some("renegotiate".into());
            session.note = "Host received a renegotiation request.".into();
        }
    }
}

async fn handle_host_signal(
    state: &ServerState,
    requester_sender: &mpsc::UnboundedSender<Message>,
    from: &str,
    payload: SignalPayload,
) {
    update_host_session(state, from, &payload).await;

    match payload {
        SignalPayload::Offer { sdp } => {
            let dispatch: SignalDispatch = {
                let requester_sender = requester_sender.clone();
                Arc::new(move |message: SignalMessage| {
                    let requester_sender = requester_sender.clone();
                    Box::pin(async move {
                        let _ = requester_sender.send(Message::Text(
                            serde_json::to_string(&message).unwrap().into(),
                        ));
                    })
                })
            };

            match HostWebRtcEngine::from_offer(
                HOST_PEER_ID,
                from.to_string(),
                sdp,
                state.preview.latest_frame_path.clone(),
                state.host_session.clone(),
                dispatch,
            )
            .await
            {
                Ok(engine) => {
                    *state.host_engine.lock().await = Some(engine);
                }
                Err(err) => {
                    let _ = requester_sender.send(Message::Text(
                        serde_json::to_string(&SignalMessage::Error {
                            message: format!("failed to create host answer: {err}"),
                        })
                        .unwrap()
                        .into(),
                    ));
                }
            }
        }
        SignalPayload::IceCandidate {
            candidate,
            sdp_mid,
            sdp_mline_index,
        } => {
            let engine = state.host_engine.lock().await;
            if let Some(engine) = engine.as_ref() {
                let candidate = webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                    candidate,
                    sdp_mid,
                    sdp_mline_index,
                    username_fragment: None,
                };
                if let Err(err) = engine
                    .add_remote_ice_candidate(candidate, state.host_session.clone())
                    .await
                {
                    let _ = requester_sender.send(Message::Text(
                        serde_json::to_string(&SignalMessage::Error {
                            message: format!("failed to add remote ICE candidate: {err}"),
                        })
                        .unwrap()
                        .into(),
                    ));
                }
            }
        }
        SignalPayload::Answer { .. } | SignalPayload::Renegotiate => {}
    }
}

fn serve_bytes(path: &Path, content_type: &'static str) -> Response {
    match fs::read(path) {
        Ok(bytes) => with_content_type(bytes, content_type).into_response(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => not_found("artifact not found"),
        Err(err) => internal_error(err),
    }
}

fn with_content_type<T: IntoResponse>(response: T, content_type: &'static str) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, no-cache, must-revalidate"));
    response
}

fn with_no_store<T: IntoResponse>(response: T) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, no-cache, must-revalidate"));
    response
}

fn not_found(message: &str) -> Response {
    (StatusCode::NOT_FOUND, message.to_string()).into_response()
}

fn internal_error(err: io::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("internal error: {err}"),
    )
        .into_response()
}

#[allow(dead_code)]
fn _json_error(message: impl Into<String>) -> Response {
    with_no_store(Json(SignalMessage::Error {
        message: message.into(),
    }))
}
