use std::env;
#[cfg(feature = "wayland-pipewire")]
use std::fs;
#[cfg(feature = "wayland-pipewire")]
use std::io;
#[cfg(feature = "wayland-pipewire")]
use std::io::Read;
#[cfg(feature = "wayland-pipewire")]
use std::os::fd::AsRawFd;
#[cfg(feature = "wayland-pipewire")]
use std::os::fd::OwnedFd;
#[cfg(feature = "wayland-pipewire")]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(feature = "wayland-pipewire")]
use std::process::Stdio;
#[cfg(feature = "wayland-pipewire")]
use std::thread;
#[cfg(feature = "wayland-pipewire")]
use std::time::{Duration, Instant};

#[cfg(feature = "wayland-pipewire")]
use ashpd::desktop::{
    PersistMode,
    screencast::{
        CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
        StartCastOptions,
    },
    Session,
};
#[cfg(feature = "wayland-pipewire")]
use enumflags2::BitFlags;
use thiserror::Error;
#[cfg(feature = "wayland-pipewire")]
use tokio::runtime::{Builder, Runtime};
#[cfg(feature = "wayland-pipewire")]
use tracing::info;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

use crate::config::CaptureConfig;

use super::{CaptureReadiness, DisplayConfig};

pub struct WaylandWlrBackend {
    #[allow(dead_code)]
    config: DisplayConfig,
    #[cfg(feature = "wayland-pipewire")]
    portal_session: PortalVirtualMonitorSession,
}

#[derive(Debug, Clone)]
pub struct WaylandEnvironment {
    #[allow(dead_code)]
    pub wayland_display: String,
    #[allow(dead_code)]
    pub runtime_dir: PathBuf,
    pub socket_path: PathBuf,
    pub supports_wlr_output_management: bool,
    pub supports_portal_virtual_monitor: bool,
    pub advertised_globals: Vec<String>,
    pub capture_stack: WaylandCaptureStack,
}

#[derive(Debug)]
struct RegistryState;

#[derive(Debug, Clone)]
pub struct WaylandCaptureStack {
    pub pipewire_feature_enabled: bool,
    #[allow(dead_code)]
    pub portal_capture_ready: bool,
    pub note: String,
}

#[cfg(feature = "wayland-pipewire")]
struct PortalVirtualMonitorSession {
    runtime: Runtime,
    session: Session<Screencast>,
    #[allow(dead_code)]
    streams: Vec<PortalStreamInfo>,
    #[allow(dead_code)]
    pipewire_remote: OwnedFd,
}

#[cfg(feature = "wayland-pipewire")]
#[derive(Debug)]
struct PortalStreamInfo {
    node_id: u32,
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
    source_type: Option<u32>,
    id: Option<String>,
    mapping_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum WaylandWlrError {
    #[error("WAYLAND_DISPLAY is not set; wlr-virtual-output backend cannot start")]
    MissingWaylandDisplay,
    #[error("XDG_RUNTIME_DIR is not set; wlr-virtual-output backend cannot find the compositor socket")]
    MissingRuntimeDir,
    #[error("Wayland compositor socket does not exist at {0}")]
    MissingSocket(PathBuf),
    #[error("failed to connect to Wayland compositor: {0}")]
    Connection(String),
    #[error("failed to inspect Wayland globals: {0}")]
    Registry(String),
    #[allow(dead_code)]
    #[error("Wayland compositor does not advertise zwlr_output_manager_v1; wlroots output-management path is unavailable")]
    MissingWlrOutputManagement,
    #[error("ScreenCast portal does not advertise VIRTUAL source support; compositor-native virtual monitor creation is unavailable")]
    MissingPortalVirtualMonitor,
    #[error("Wayland virtual output exists, but the PipeWire / portal capture stack is not compiled in; enable the `wayland-pipewire` feature")]
    MissingPipeWireFeature,
    #[cfg(feature = "wayland-pipewire")]
    #[error("failed to start Wayland ScreenCast portal session: {0}")]
    PortalSession(String),
    #[cfg(feature = "wayland-pipewire")]
    #[error("Wayland ScreenCast portal started but returned no streams for the virtual monitor")]
    PortalReturnedNoStreams,
    #[cfg(feature = "wayland-pipewire")]
    #[error("gst-launch-1.0 with pipewiresrc is required for the current Wayland capture bridge")]
    GStreamerUnavailable,
    #[cfg(feature = "wayland-pipewire")]
    #[error("failed to create Wayland capture output directory: {0}")]
    FrameWriteFailed(#[source] io::Error),
    #[cfg(feature = "wayland-pipewire")]
    #[error("Wayland PipeWire capture process failed: {0}")]
    CaptureSpawnFailed(#[source] io::Error),
    #[cfg(feature = "wayland-pipewire")]
    #[error("Wayland PipeWire capture pipeline failed: {0}")]
    CapturePipelineFailed(String),
    #[cfg(feature = "wayland-pipewire")]
    #[error("timed out waiting for the Wayland PipeWire capture pipeline to produce frames")]
    CaptureTimedOut,
    #[cfg(not(feature = "wayland-pipewire"))]
    #[error("Wayland virtual monitor exists, but PNG frame extraction has not been completed for this backend yet")]
    CaptureNotImplemented,
}

impl WaylandWlrBackend {
    pub fn start(_config: DisplayConfig) -> Result<Self, WaylandWlrError> {
        let env = detect_wayland_environment()?;
        if !env.supports_portal_virtual_monitor {
            return Err(WaylandWlrError::MissingPortalVirtualMonitor);
        }
        if !env.capture_stack.pipewire_feature_enabled {
            return Err(WaylandWlrError::MissingPipeWireFeature);
        }
        #[cfg(feature = "wayland-pipewire")]
        {
            let portal_session = establish_virtual_monitor_session()?;
            let primary_stream = portal_session.streams.first();
            info!(
                streams = portal_session.streams.len(),
                primary_node_id = primary_stream.map(|stream| stream.node_id),
                primary_position = ?primary_stream.and_then(|stream| stream.position),
                primary_size = ?primary_stream.and_then(|stream| stream.size),
                primary_source_type = ?primary_stream.and_then(|stream| stream.source_type),
                primary_stream_id = ?primary_stream.and_then(|stream| stream.id.clone()),
                primary_mapping_id = ?primary_stream.and_then(|stream| stream.mapping_id.clone()),
                "started Wayland virtual monitor portal session"
            );
            return Ok(Self {
                config: _config,
                portal_session,
            });
        }

        #[cfg(not(feature = "wayland-pipewire"))]
        {
            let _ = _config;
            Err(WaylandWlrError::MissingPipeWireFeature)
        }
    }

    pub fn capture_frames_to_png(
        &mut self,
        capture: &CaptureConfig,
    ) -> Result<Vec<PathBuf>, WaylandWlrError> {
        #[cfg(feature = "wayland-pipewire")]
        {
            capture_frames_via_gstreamer(&self.portal_session, capture)
        }

        #[cfg(not(feature = "wayland-pipewire"))]
        {
            let _ = capture;
            Err(WaylandWlrError::CaptureNotImplemented)
        }
    }

    pub fn capture_readiness(&self) -> CaptureReadiness {
        #[cfg(feature = "wayland-pipewire")]
        {
            return CaptureReadiness::Ready;
        }

        #[cfg(not(feature = "wayland-pipewire"))]
        {
            CaptureReadiness::Pending(
                "virtual monitor creation requires the wayland-pipewire feature for capture wiring"
                    .into(),
            )
        }
    }

    pub fn external_monitor_summary(&self) -> String {
        #[cfg(feature = "wayland-pipewire")]
        {
            let streams = self
                .portal_session
                .streams
                .iter()
                .map(|stream| {
                    format!(
                        "node_id={} id={:?} mapping_id={:?} position={:?} size={:?} source_type={:?}",
                        stream.node_id,
                        stream.id,
                        stream.mapping_id,
                        stream.position,
                        stream.size,
                        stream.source_type
                    )
                })
                .collect::<Vec<_>>();
            return format!(
                "Wayland virtual monitor session active; requested={}x{}@{}; streams=[{}]",
                self.config.width,
                self.config.height,
                self.config.refresh_hz,
                streams.join("; ")
            );
        }

        #[cfg(not(feature = "wayland-pipewire"))]
        {
            format!(
                "Wayland virtual monitor backend selected for {}x{}@{}, but the portal capture feature is not compiled in",
                self.config.width, self.config.height, self.config.refresh_hz
            )
        }
    }
}

#[cfg(feature = "wayland-pipewire")]
impl Drop for WaylandWlrBackend {
    fn drop(&mut self) {
        let _ = self.portal_session.runtime.block_on(self.portal_session.session.close());
    }
}

pub fn detect_wayland_environment() -> Result<WaylandEnvironment, WaylandWlrError> {
    let wayland_display =
        env::var("WAYLAND_DISPLAY").map_err(|_| WaylandWlrError::MissingWaylandDisplay)?;
    let runtime_dir = env::var("XDG_RUNTIME_DIR").map_err(|_| WaylandWlrError::MissingRuntimeDir)?;
    let socket_path = Path::new(&runtime_dir).join(&wayland_display);

    if !socket_path.exists() {
        return Err(WaylandWlrError::MissingSocket(socket_path));
    }

    let connection =
        Connection::connect_to_env().map_err(|err| WaylandWlrError::Connection(err.to_string()))?;
    let (globals, mut queue) =
        registry_queue_init::<RegistryState>(&connection).map_err(|err| {
            WaylandWlrError::Registry(err.to_string())
        })?;
    queue
        .roundtrip(&mut RegistryState)
        .map_err(|err| WaylandWlrError::Registry(err.to_string()))?;

    let advertised_globals = globals
        .contents()
        .clone_list()
        .into_iter()
        .map(|global| global.interface)
        .collect::<Vec<_>>();
    let supports_wlr_output_management = advertised_globals
        .iter()
        .any(|interface| interface == "zwlr_output_manager_v1");
    let supports_portal_virtual_monitor = detect_portal_virtual_monitor_support().unwrap_or(false);
    let capture_stack = detect_capture_stack();

    Ok(WaylandEnvironment {
        wayland_display,
        runtime_dir: PathBuf::from(runtime_dir),
        socket_path,
        supports_wlr_output_management,
        supports_portal_virtual_monitor,
        advertised_globals,
        capture_stack,
    })
}

fn detect_capture_stack() -> WaylandCaptureStack {
    #[cfg(feature = "wayland-pipewire")]
    {
        WaylandCaptureStack {
            pipewire_feature_enabled: true,
            portal_capture_ready: true,
            note: "ashpd + zbus virtual-monitor portal session support is compiled in, and PNG frame extraction is bridged through GStreamer pipewiresrc".into(),
        }
    }

    #[cfg(not(feature = "wayland-pipewire"))]
    {
        WaylandCaptureStack {
            pipewire_feature_enabled: false,
            portal_capture_ready: false,
            note: "PipeWire / portal stack is not compiled in yet; enable the `wayland-pipewire` feature".into(),
        }
    }
}

fn detect_portal_virtual_monitor_support() -> Result<bool, WaylandWlrError> {
    let output = Command::new("busctl")
        .args([
            "--user",
            "get-property",
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.ScreenCast",
            "AvailableSourceTypes",
        ])
        .output()
        .map_err(|err| WaylandWlrError::Registry(err.to_string()))?;

    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut parts = stdout.split_whitespace();
    let ty = parts.next().unwrap_or_default();
    let value = parts.next().unwrap_or_default();
    if ty != "u" {
        return Ok(false);
    }

    let bitmask = value
        .parse::<u32>()
        .map_err(|err| WaylandWlrError::Registry(err.to_string()))?;

    Ok((bitmask & 4) != 0)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for RegistryState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<RegistryState>,
    ) {
    }
}

#[cfg(feature = "wayland-pipewire")]
fn establish_virtual_monitor_session() -> Result<PortalVirtualMonitorSession, WaylandWlrError> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| WaylandWlrError::PortalSession(err.to_string()))?;

    let (session, streams, pipewire_remote) = runtime.block_on(async {
        let proxy = Screencast::new()
            .await
            .map_err(|err| WaylandWlrError::PortalSession(err.to_string()))?;
        let available = proxy
            .available_source_types()
            .await
            .map_err(|err| WaylandWlrError::PortalSession(err.to_string()))?;

        if !available.contains(SourceType::Virtual) {
            return Err(WaylandWlrError::MissingPortalVirtualMonitor);
        }

        let session = proxy
            .create_session(Default::default())
            .await
            .map_err(|err| WaylandWlrError::PortalSession(err.to_string()))?;

        proxy
            .select_sources(
                &session,
                SelectSourcesOptions::default()
                    .set_cursor_mode(CursorMode::Hidden)
                    .set_sources(BitFlags::from_flag(SourceType::Virtual))
                    .set_multiple(false)
                    .set_persist_mode(PersistMode::DoNot),
            )
            .await
            .map_err(|err| WaylandWlrError::PortalSession(err.to_string()))?;

        let response = proxy
            .start(&session, None, StartCastOptions::default())
            .await
            .map_err(|err| WaylandWlrError::PortalSession(err.to_string()))?
            .response()
            .map_err(|err| WaylandWlrError::PortalSession(err.to_string()))?;

        let streams = response
            .streams()
            .iter()
            .map(|stream| PortalStreamInfo {
                node_id: stream.pipe_wire_node_id(),
                position: stream.position(),
                size: stream.size(),
                source_type: stream.source_type().map(|value| value as u32),
                id: stream.id().map(ToOwned::to_owned),
                mapping_id: stream.mapping_id().map(ToOwned::to_owned),
            })
            .collect::<Vec<_>>();

        if streams.is_empty() {
            return Err(WaylandWlrError::PortalReturnedNoStreams);
        }

        let pipewire_remote = proxy
            .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
            .await
            .map_err(|err| WaylandWlrError::PortalSession(err.to_string()))?;

        Ok((session, streams, pipewire_remote))
    })?;

    Ok(PortalVirtualMonitorSession {
        runtime,
        session,
        streams,
        pipewire_remote,
    })
}

#[cfg(feature = "wayland-pipewire")]
fn capture_frames_via_gstreamer(
    session: &PortalVirtualMonitorSession,
    capture: &CaptureConfig,
) -> Result<Vec<PathBuf>, WaylandWlrError> {
    if capture.frames == 0 {
        return Ok(Vec::new());
    }

    let primary_stream = session
        .streams
        .first()
        .ok_or(WaylandWlrError::PortalReturnedNoStreams)?;
    let output_dir = Path::new(&capture.output_dir);
    fs::create_dir_all(output_dir).map_err(WaylandWlrError::FrameWriteFailed)?;
    purge_existing_frames(output_dir).map_err(WaylandWlrError::FrameWriteFailed)?;

    const GST_PIPEWIRE_FD: i32 = 198;
    let mut command = Command::new("gst-launch-1.0");
    command
        .arg("-q")
        .arg("pipewiresrc")
        .arg(format!("fd={GST_PIPEWIRE_FD}"))
        .arg(format!("path={}", primary_stream.node_id))
        .arg(format!("num-buffers={}", capture.frames))
        .arg("always-copy=true")
        .arg("keepalive-time=100")
        .arg("do-timestamp=true")
        .arg("!")
        .arg("videoconvert")
        .arg("!")
        .arg("video/x-raw,format=RGBA")
        .arg("!")
        .arg("pngenc")
        .arg("snapshot=false")
        .arg("!")
        .arg("multifilesink")
        .arg(format!(
            "location={}",
            output_dir.join("frame-%04d.png").display()
        ))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let remote_fd = session.pipewire_remote.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(remote_fd, GST_PIPEWIRE_FD) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(WaylandWlrError::GStreamerUnavailable);
        }
        Err(err) => return Err(WaylandWlrError::CaptureSpawnFailed(err)),
    };

    let deadline = Instant::now() + Duration::from_secs(capture.max_wait_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr = read_child_stderr(&mut child);
                if !status.success() {
                    let detail = if stderr.trim().is_empty() {
                        format!("gst-launch exited with status {status}")
                    } else {
                        stderr
                    };
                    return Err(WaylandWlrError::CapturePipelineFailed(detail));
                }
                break;
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let stderr = read_child_stderr(&mut child);
                if !stderr.trim().is_empty() {
                    return Err(WaylandWlrError::CapturePipelineFailed(stderr));
                }
                return Err(WaylandWlrError::CaptureTimedOut);
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(err) => return Err(WaylandWlrError::CaptureSpawnFailed(err)),
        }
    }

    let mut frames = fs::read_dir(output_dir)
        .map_err(WaylandWlrError::FrameWriteFailed)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("frame-") && name.ends_with(".png"))
        })
        .collect::<Vec<_>>();
    frames.sort();

    if frames.is_empty() {
        return Err(WaylandWlrError::CaptureTimedOut);
    }

    Ok(frames)
}

#[cfg(feature = "wayland-pipewire")]
fn purge_existing_frames(output_dir: &Path) -> Result<(), io::Error> {
    if !output_dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(output_dir)? {
        let entry = entry?;
        let path = entry.path();
        let should_remove = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("frame-") && name.ends_with(".png"));
        if should_remove {
            fs::remove_file(path)?;
        }
    }

    Ok(())
}

#[cfg(feature = "wayland-pipewire")]
fn read_child_stderr(child: &mut std::process::Child) -> String {
    let mut stderr = String::new();
    if let Some(mut handle) = child.stderr.take() {
        let _ = handle.read_to_string(&mut stderr);
    }
    stderr.trim().to_string()
}
