use std::future::Future;
use std::io::Read;
use std::pin::Pin;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use webrtc::api::media_engine::{MIME_TYPE_H264, MediaEngine};
use webrtc::api::APIBuilder;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;
use webrtc_media::Sample;

use crate::display::LiveVideoSource;
use crate::input::{ClientInputMessage, SharedInputInjector};

pub type SignalDispatch = Arc<
    dyn Fn(SignalMessage) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static,
>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalMessage {
    Hello {
        peer_id: String,
        role: PeerRole,
        capabilities: Option<PeerCapabilities>,
    },
    Peers {
        peers: Vec<PeerSummary>,
    },
    Signal {
        from: String,
        to: String,
        payload: SignalPayload,
    },
    HostSession {
        session: HostSessionState,
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerRole {
    Host,
    BrowserClient,
    MobileClient,
    DesktopClient,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCapabilities {
    pub wants_video: bool,
    pub wants_input: bool,
    pub max_resolution: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSummary {
    pub peer_id: String,
    pub role: PeerRole,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostSessionState {
    pub peer_id: String,
    pub last_offer_from: Option<String>,
    pub pending_offer: bool,
    pub pending_ice_candidates: usize,
    pub video_samples_sent: usize,
    pub last_video_error: Option<String>,
    pub last_signal_kind: Option<String>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalPayload {
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
    Renegotiate,
}

pub struct HostWebRtcEngine {
    peer_connection: Arc<RTCPeerConnection>,
    remote_peer_id: String,
    #[allow(dead_code)]
    video_track: Arc<TrackLocalStaticSample>,
    video_shutdown: Arc<AtomicBool>,
}

#[derive(Debug, Error)]
pub enum HostWebRtcError {
    #[error("failed to initialize media engine: {0}")]
    MediaEngine(#[from] webrtc::Error),
}

impl HostWebRtcEngine {
    pub async fn from_offer(
        host_peer_id: &'static str,
        remote_peer_id: String,
        offer_sdp: String,
        latest_frame_path: PathBuf,
        sample_interval: Duration,
        live_video_source: Option<LiveVideoSource>,
        host_session: Arc<Mutex<HostSessionState>>,
        input_injector: SharedInputInjector,
        dispatch_signal: SignalDispatch,
    ) -> Result<Self, HostWebRtcError> {
        let mut media_engine = MediaEngine::default();
        media_engine.register_default_codecs()?;
        let api = APIBuilder::new().with_media_engine(media_engine).build();

        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.l.google.com:19302".into()],
                ..Default::default()
            }],
            ..Default::default()
        };

        let peer_connection = Arc::new(api.new_peer_connection(config).await?);
        let video_track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_owned(),
                ..Default::default()
            },
            "video".to_owned(),
            "desplio".to_owned(),
        ));

        let rtp_sender = peer_connection
            .add_track(video_track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;

        tokio::spawn(async move {
            let mut rtcp_buffer = vec![0u8; 1500];
            while rtp_sender.read(&mut rtcp_buffer).await.is_ok() {}
        });

        {
            let dispatch = dispatch_signal.clone();
            let remote_peer_id = remote_peer_id.clone();
            peer_connection.on_ice_candidate(Box::new(move |candidate: Option<RTCIceCandidate>| {
                let dispatch = dispatch.clone();
                let remote_peer_id = remote_peer_id.clone();
                Box::pin(async move {
                    if let Some(candidate) = candidate {
                        match candidate.to_json() {
                            Ok(json) => {
                                (dispatch)(SignalMessage::Signal {
                                    from: host_peer_id.into(),
                                    to: remote_peer_id.clone(),
                                    payload: SignalPayload::IceCandidate {
                                        candidate: json.candidate,
                                        sdp_mid: json.sdp_mid,
                                        sdp_mline_index: json.sdp_mline_index,
                                    },
                                })
                                .await;
                            }
                            Err(err) => {
                                debug!(error = %err, "failed to serialize host ICE candidate");
                            }
                        }
                    }
                })
            }));
        }

        {
            let host_session = host_session.clone();
            peer_connection.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
                let host_session = host_session.clone();
                Box::pin(async move {
                    let mut session = host_session.lock().await;
                    session.note = format!("Host peer connection state: {state}");
                })
            }));
        }

        {
            let host_session = host_session.clone();
            let input_injector = input_injector.clone();
            peer_connection.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
                let host_session = host_session.clone();
                let input_injector = input_injector.clone();
                Box::pin(async move {
                    install_host_data_channel_handlers(channel, host_session, input_injector).await;
                })
            }));
        }

        peer_connection.set_remote_description(RTCSessionDescription::offer(offer_sdp)?).await?;

        let answer = peer_connection.create_answer(None).await?;
        peer_connection.set_local_description(answer.clone()).await?;

        {
            let mut session = host_session.lock().await;
            session.pending_offer = false;
            session.last_signal_kind = Some("answer".into());
            session.note = "Host generated an SDP answer and started ICE gathering.".into();
        }

        (dispatch_signal)(SignalMessage::Signal {
            from: host_peer_id.into(),
            to: remote_peer_id.clone(),
            payload: SignalPayload::Answer { sdp: answer.sdp },
        })
        .await;

        info!(remote_peer_id, "host generated WebRTC answer");

        let video_shutdown = start_video_producer(
            video_track.clone(),
            latest_frame_path,
            sample_interval,
            live_video_source,
            host_session.clone(),
        );

        Ok(Self {
            peer_connection,
            remote_peer_id,
            video_track,
            video_shutdown,
        })
    }

    pub async fn add_remote_ice_candidate(
        &self,
        candidate: RTCIceCandidateInit,
        host_session: Arc<Mutex<HostSessionState>>,
    ) -> Result<(), HostWebRtcError> {
        self.peer_connection.add_ice_candidate(candidate).await?;
        let mut session = host_session.lock().await;
        session.note = format!("Host accepted remote ICE candidate from {}", self.remote_peer_id);
        Ok(())
    }

    pub async fn shutdown(self) {
        self.video_shutdown.store(true, Ordering::SeqCst);
        if let Err(err) = self.peer_connection.close().await {
            debug!(error = %err, "failed to close old host peer connection");
        }
    }
}

fn start_video_producer(
    track: Arc<TrackLocalStaticSample>,
    latest_frame_path: PathBuf,
    sample_interval: Duration,
    live_video_source: Option<LiveVideoSource>,
    host_session: Arc<Mutex<HostSessionState>>,
) -> Arc<AtomicBool> {
    let shutdown = Arc::new(AtomicBool::new(false));
    match live_video_source {
        Some(source @ LiveVideoSource::X11Grab { .. }) => {
            run_x11grab_video_loop(
                track,
                source,
                sample_interval,
                host_session,
                shutdown.clone(),
            );
        }
        None => {
            tokio::spawn(run_latest_frame_video_loop(
                track,
                latest_frame_path,
                sample_interval,
                host_session,
                shutdown.clone(),
            ));
        }
    }
    shutdown
}

fn run_x11grab_video_loop(
    track: Arc<TrackLocalStaticSample>,
    source: LiveVideoSource,
    sample_interval: Duration,
    host_session: Arc<Mutex<HostSessionState>>,
    shutdown: Arc<AtomicBool>,
) {
    let LiveVideoSource::X11Grab {
        display,
        x,
        y,
        width,
        height,
    } = source;

    let handle = tokio::runtime::Handle::current();
    thread::Builder::new()
        .name("desplio-x11grab-webrtc".into())
        .spawn(move || {
            let input = format!("{display}+{x},{y}");
            let video_size = format!("{width}x{height}");
            let fps = (1000 / sample_interval.as_millis().max(1) as u64).clamp(1, 30);
            let keyint = fps.max(1).to_string();

            let mut child = match Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-fflags",
                    "nobuffer",
                    "-flags",
                    "low_delay",
                    "-probesize",
                    "32",
                    "-analyzeduration",
                    "0",
                    "-f",
                    "x11grab",
                    "-framerate",
                    &fps.to_string(),
                    "-video_size",
                    &video_size,
                    "-i",
                    &input,
                    "-an",
                    "-c:v",
                    "libx264",
                    "-profile:v",
                    "baseline",
                    "-level",
                    "3.1",
                    "-preset",
                    "ultrafast",
                    "-tune",
                    "zerolatency",
                    "-x264-params",
                    &format!("repeat-headers=1:annexb=1:keyint={keyint}:min-keyint={keyint}:scenecut=0:rc-lookahead=0:sync-lookahead=0"),
                    "-pix_fmt",
                    "yuv420p",
                    "-bf",
                    "0",
                    "-threads",
                    "1",
                    "-flush_packets",
                    "1",
                    "-f",
                    "h264",
                    "-",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(err) => {
                    handle.block_on(record_video_error(
                        host_session.clone(),
                        format!("failed to start x11grab encoder: {err}"),
                    ));
                    return;
                }
            };

            let Some(mut stdout) = child.stdout.take() else {
                handle.block_on(record_video_error(
                    host_session.clone(),
                    "x11grab encoder did not expose stdout".into(),
                ));
                let _ = child.kill();
                let _ = child.wait();
                return;
            };

            let mut parser = AnnexBAccessUnitParser::default();
            let mut buffer = [0u8; 16 * 1024];
            let mut samples_sent = 0usize;

            loop {
                if shutdown.load(Ordering::SeqCst) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }

                let read = match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(err) => {
                        handle.block_on(record_video_error(
                            host_session.clone(),
                            format!("failed to read x11grab stream: {err}"),
                        ));
                        break;
                    }
                };

                if shutdown.load(Ordering::SeqCst) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }

                let Some(access_unit) = parser.push(&buffer[..read]).into_iter().last() else {
                    continue;
                };

                {
                    if access_unit.is_empty() {
                        continue;
                    }

                    if shutdown.load(Ordering::SeqCst) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }

                    let sample = Sample {
                        data: Bytes::from(access_unit),
                        duration: sample_interval,
                        ..Default::default()
                    };

                    let write_result = handle.block_on(track.write_sample(&sample));
                    if let Err(err) = write_result {
                        handle.block_on(record_video_error(
                            host_session.clone(),
                            format!("failed to write x11grab WebRTC sample: {err}"),
                        ));
                        let _ = child.kill();
                        let _ = child.wait();
                        return;
                    }

                    samples_sent += 1;
                    handle.block_on(record_video_sample(
                        host_session.clone(),
                        samples_sent,
                        "Host is streaming direct X11 capture over a WebRTC H.264 video track.",
                    ));
                }
            }

            let _ = child.kill();
            let _ = child.wait();
        })
        .expect("failed to spawn x11grab WebRTC producer");
}

async fn record_video_sample(
    host_session: Arc<Mutex<HostSessionState>>,
    samples_sent: usize,
    note: &'static str,
) {
    let mut session = host_session.lock().await;
    session.video_samples_sent = samples_sent;
    session.last_video_error = None;
    session.note = note.into();
}

async fn record_video_error(host_session: Arc<Mutex<HostSessionState>>, message: String) {
    let mut session = host_session.lock().await;
    session.last_video_error = Some(message.clone());
    session.note = message;
}

#[derive(Default)]
struct AnnexBAccessUnitParser {
    buffer: Vec<u8>,
    current_access_unit: Vec<u8>,
    current_has_vcl: bool,
}

impl AnnexBAccessUnitParser {
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        let mut completed = Vec::new();

        while let Some((start, prefix_len)) = find_start_code(&self.buffer) {
            if start > 0 {
                self.buffer.drain(..start);
                continue;
            }

            let Some((next_start, _)) = find_start_code(&self.buffer[prefix_len..])
                .map(|(relative_start, relative_prefix_len)| {
                    (prefix_len + relative_start, relative_prefix_len)
                })
            else {
                break;
            };

            let nal = self.buffer[prefix_len..next_start].to_vec();
            self.buffer.drain(..next_start);
            if nal.is_empty() {
                continue;
            }

            if let Some(access_unit) = self.push_nal(nal) {
                completed.push(access_unit);
            }
        }

        completed
    }

    fn push_nal(&mut self, nal: Vec<u8>) -> Option<Vec<u8>> {
        let nal_type = nal[0] & 0x1f;
        let starts_new_access_unit = self.current_has_vcl && starts_access_unit_after_vcl(nal_type);

        let completed = if starts_new_access_unit && !self.current_access_unit.is_empty() {
            self.current_has_vcl = false;
            Some(std::mem::take(&mut self.current_access_unit))
        } else {
            None
        };

        self.current_access_unit.extend_from_slice(&[0, 0, 0, 1]);
        self.current_access_unit.extend_from_slice(&nal);
        if nal_type == 1 || nal_type == 5 {
            self.current_has_vcl = true;
        }

        completed
    }
}

fn starts_access_unit_after_vcl(nal_type: u8) -> bool {
    matches!(
        nal_type,
        1 | 5 | 6 | 7 | 8 | 9 | 14 | 15 | 16 | 18 | 20 | 21
    )
}

fn find_start_code(bytes: &[u8]) -> Option<(usize, usize)> {
    for index in 0..bytes.len().saturating_sub(3) {
        if bytes[index] == 0 && bytes[index + 1] == 0 {
            if bytes[index + 2] == 1 {
                return Some((index, 3));
            }
            if index + 3 < bytes.len() && bytes[index + 2] == 0 && bytes[index + 3] == 1 {
                return Some((index, 4));
            }
        }
    }
    None
}

async fn run_latest_frame_video_loop(
    track: Arc<TrackLocalStaticSample>,
    latest_frame_path: PathBuf,
    sample_interval: Duration,
    host_session: Arc<Mutex<HostSessionState>>,
    shutdown: Arc<AtomicBool>,
) {
    let mut last_payload: Option<Vec<u8>> = None;
    let mut last_frame_modified: Option<std::time::SystemTime> = None;
    let mut samples_sent = 0usize;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        let encoded = match std::fs::metadata(&latest_frame_path)
            .and_then(|metadata| metadata.modified())
        {
            Ok(modified) if last_payload.is_some() && Some(modified) == last_frame_modified => {
                Ok(last_payload.clone().unwrap_or_default())
            }
            Ok(modified) => {
                let encoded = crate::encoder::encode_png_to_h264_annexb(&latest_frame_path);
                if encoded.is_ok() {
                    last_frame_modified = Some(modified);
                }
                encoded
            }
            Err(err) => Err(crate::encoder::EncodeError::EncodeFailed(format!(
                "failed to stat latest frame: {err}"
            ))),
        };

        match encoded {
            Ok(payload) if !payload.is_empty() => {
                let sample = Sample {
                    data: Bytes::from(payload.clone()),
                    duration: sample_interval,
                    ..Default::default()
                };

                if let Err(err) = track.write_sample(&sample).await {
                    warn!(error = %err, "failed to write WebRTC video sample");
                    break;
                }

                last_payload = Some(payload);
                samples_sent += 1;
                let mut session = host_session.lock().await;
                session.video_samples_sent = samples_sent;
                session.last_video_error = None;
                session.note =
                    "Host is streaming the latest captured frame over a real WebRTC H.264 video track.".into();
            }
            Ok(_) => {
                let mut session = host_session.lock().await;
                session.last_video_error = Some("latest-frame encoded to an empty H.264 payload".into());
                warn!("latest-frame artifact encoded to an empty H.264 payload");
            }
            Err(err) => {
                if let Some(payload) = last_payload.clone() {
                    let sample = Sample {
                        data: Bytes::from(payload),
                        duration: sample_interval,
                        ..Default::default()
                    };
                    if let Err(write_err) = track.write_sample(&sample).await {
                        let mut session = host_session.lock().await;
                        session.last_video_error = Some(format!("failed to repeat previous sample: {write_err}"));
                        warn!(error = %write_err, "failed to repeat previous WebRTC video sample");
                        break;
                    }
                    samples_sent += 1;
                    let mut session = host_session.lock().await;
                    session.video_samples_sent = samples_sent;
                    session.last_video_error = Some(format!("repeated previous sample after encode error: {err}"));
                } else {
                    let mut session = host_session.lock().await;
                    session.last_video_error = Some(format!("failed to encode latest frame: {err}"));
                    warn!(error = %err, path = %latest_frame_path.display(), "failed to encode latest frame for WebRTC video track");
                }
            }
        }

        tokio::time::sleep(sample_interval).await;
    }
}

async fn install_host_data_channel_handlers(
    channel: Arc<RTCDataChannel>,
    host_session: Arc<Mutex<HostSessionState>>,
    input_injector: SharedInputInjector,
) {
    let label = channel.label().to_string();

    {
        let mut session = host_session.lock().await;
        session.note = format!("Host received data channel '{label}' and is waiting for it to open.");
    }

    {
        let host_session = host_session.clone();
        let open_channel = channel.clone();
        let label_for_open = label.clone();
        channel.on_open(Box::new(move || {
            let host_session = host_session.clone();
            let channel = open_channel.clone();
            let label_for_open = label_for_open.clone();
            Box::pin(async move {
                {
                    let mut session = host_session.lock().await;
                    session.note = format!("Host data channel '{label_for_open}' is open.");
                }

                if let Err(err) = channel.send_text("desplio-host ready").await {
                    debug!(error = %err, "failed to send host data channel greeting");
                }
            })
        }));
    }

    {
        let host_session = host_session.clone();
        let message_channel = channel.clone();
        let label_for_message = label.clone();
        let input_injector = input_injector.clone();
        channel.on_message(Box::new(move |msg: DataChannelMessage| {
            let host_session = host_session.clone();
            let channel = message_channel.clone();
            let label_for_message = label_for_message.clone();
            let input_injector = input_injector.clone();
            Box::pin(async move {
                let text = String::from_utf8_lossy(&msg.data).to_string();
                if let Ok(input_message) = serde_json::from_str::<ClientInputMessage>(&text) {
                    let result = {
                        let mut guard = input_injector.lock().expect("input injector lock poisoned");
                        guard
                            .as_mut()
                            .map(|injector| injector.handle_client_message(input_message))
                    };

                    match result {
                        Some(Ok(())) => {
                            let mut session = host_session.lock().await;
                            session.note = "Host injected M5 pointer input from client.".into();
                        }
                        Some(Err(err)) => {
                            let mut session = host_session.lock().await;
                            session.note = format!("Host failed to inject M5 input: {err}");
                        }
                        None => {
                            let mut session = host_session.lock().await;
                            session.note =
                                "Host received M5 input but no input injector is available.".into();
                        }
                    }
                    return;
                }

                {
                    let mut session = host_session.lock().await;
                    session.note = format!(
                        "Host data channel '{label_for_message}' received: {text}"
                    );
                }

                if let Err(err) = channel.send_text(format!("ack:{text}")).await {
                    debug!(error = %err, "failed to echo data channel message back to browser");
                }
            })
        }));
    }
}
