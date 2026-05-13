use std::env;
#[cfg(feature = "wayland-pipewire")]
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Command;

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

use super::DisplayConfig;

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
    #[error("Wayland frame capture is not implemented yet; PipeWire capture will land on this backend")]
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
        _capture: &CaptureConfig,
    ) -> Result<Vec<PathBuf>, WaylandWlrError> {
        Err(WaylandWlrError::CaptureNotImplemented)
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
            note: "ashpd + zbus virtual-monitor portal session support is compiled in; direct PipeWire stream consumption lands next".into(),
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
