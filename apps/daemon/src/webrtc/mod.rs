use std::future::Future;
use std::pin::Pin;
use std::path::PathBuf;
use std::sync::Arc;
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
        host_session: Arc<Mutex<HostSessionState>>,
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
            peer_connection.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
                let host_session = host_session.clone();
                Box::pin(async move {
                    install_host_data_channel_handlers(channel, host_session).await;
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

        tokio::spawn(run_latest_frame_video_loop(
            video_track.clone(),
            latest_frame_path,
            host_session.clone(),
        ));

        Ok(Self {
            peer_connection,
            remote_peer_id,
            video_track,
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
}

async fn run_latest_frame_video_loop(
    track: Arc<TrackLocalStaticSample>,
    latest_frame_path: PathBuf,
    host_session: Arc<Mutex<HostSessionState>>,
) {
    let mut last_payload: Option<Vec<u8>> = None;
    let mut samples_sent = 0usize;

    loop {
        match crate::encoder::encode_png_to_h264_annexb(&latest_frame_path) {
            Ok(payload) if !payload.is_empty() => {
                let sample = Sample {
                    data: Bytes::from(payload.clone()),
                    duration: Duration::from_millis(500),
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
                        duration: Duration::from_millis(500),
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

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn install_host_data_channel_handlers(
    channel: Arc<RTCDataChannel>,
    host_session: Arc<Mutex<HostSessionState>>,
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
        channel.on_message(Box::new(move |msg: DataChannelMessage| {
            let host_session = host_session.clone();
            let channel = message_channel.clone();
            let label_for_message = label_for_message.clone();
            Box::pin(async move {
                let text = String::from_utf8_lossy(&msg.data).to_string();
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
