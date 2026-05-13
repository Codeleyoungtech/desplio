mod edid;
mod evdi;
mod wayland_wlr;
mod x11_dummy;

use std::env;
use std::fs;
use std::path::PathBuf;

use thiserror::Error;

pub use evdi::EvdiError;
#[derive(Debug, Clone, Copy)]
pub struct DisplayConfig {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

pub enum DisplayBackend {
    Evdi(evdi::EvdiBackend),
    WaylandWlr(wayland_wlr::WaylandWlrBackend),
    X11Dummy(x11_dummy::X11DummyBackend),
}

#[derive(Debug, Clone)]
pub struct DisplayBackendInfo {
    pub selected_backend: &'static str,
    pub host: HostDisplayEnvironment,
    pub backends: Vec<BackendCapability>,
}

#[derive(Debug, Clone)]
pub struct HostDisplayEnvironment {
    pub session_type: String,
    pub display_env_present: bool,
    pub has_dri_dir: bool,
}

#[derive(Debug, Clone)]
pub struct BackendCapability {
    pub backend: &'static str,
    pub usable_now: bool,
    pub safe_for_daily_desktop: bool,
    pub supports_multiple_virtual_displays: bool,
    pub requires_free_physical_pipeline: bool,
    pub estimated_max_virtual_displays: Option<usize>,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendChoice {
    Evdi,
    WaylandWlr,
    X11Dummy,
}

#[derive(Debug, Error)]
pub enum DisplayError {
    #[error(transparent)]
    Evdi(#[from] evdi::EvdiError),
    #[error(transparent)]
    WaylandWlr(#[from] wayland_wlr::WaylandWlrError),
    #[error(transparent)]
    X11Dummy(#[from] x11_dummy::X11DummyError),
}

impl DisplayBackend {
    pub fn inspect(config: DisplayConfig) -> DisplayBackendInfo {
        inspect_backends(config)
    }

    pub fn start(config: DisplayConfig) -> Result<Self, DisplayError> {
        match choose_backend() {
            BackendChoice::Evdi => Ok(Self::Evdi(evdi::EvdiBackend::start(config)?)),
            BackendChoice::WaylandWlr => Ok(Self::WaylandWlr(wayland_wlr::WaylandWlrBackend::start(config)?)),
            BackendChoice::X11Dummy => Ok(Self::X11Dummy(x11_dummy::X11DummyBackend::start(config)?)),
        }
    }

    pub fn capture_frames_to_png(
        &mut self,
        capture: &crate::config::CaptureConfig,
    ) -> Result<Vec<PathBuf>, DisplayError> {
        match self {
            Self::Evdi(backend) => Ok(backend.capture_frames_to_png(capture)?),
            Self::WaylandWlr(backend) => Ok(backend.capture_frames_to_png(capture)?),
            Self::X11Dummy(backend) => Ok(backend.capture_frames_to_png(capture)?),
        }
    }
}

fn inspect_backends(_config: DisplayConfig) -> DisplayBackendInfo {
    let session_type = env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
    let display_env_present = env::var("DISPLAY").map(|value| !value.is_empty()).unwrap_or(false);
    let has_dri_dir = fs::metadata("/dev/dri").map(|meta| meta.is_dir()).unwrap_or(false);
    let x11_runtime_candidates = x11_dummy::runtime_output_candidates().unwrap_or_default();
    let wayland_env = wayland_wlr::detect_wayland_environment().ok();

    let wayland_backend = BackendCapability {
        backend: "wayland-wlr",
        usable_now: wayland_env
            .as_ref()
            .map(|env| env.supports_portal_virtual_monitor)
            .unwrap_or(false),
        safe_for_daily_desktop: true,
        supports_multiple_virtual_displays: true,
        requires_free_physical_pipeline: false,
        estimated_max_virtual_displays: None,
        note: match &wayland_env {
            Some(env) if env.supports_portal_virtual_monitor => format!(
                "Wayland compositor socket detected at {}; ScreenCast portal advertises VIRTUAL monitor support; wlroots output-management advertised={} ; capture stack: {}",
                env.socket_path.display(),
                env.supports_wlr_output_management,
                env.capture_stack.note
            ),
            Some(env) => format!(
                "Wayland compositor socket detected at {}, but ScreenCast portal VIRTUAL support is unavailable; wlroots output-management advertised={} ; current Wayland compositor globals: {}",
                env.socket_path.display(),
                env.supports_wlr_output_management,
                env.advertised_globals.join(", ")
            ),
            None => "No Wayland compositor socket detected for wlr-virtual-output".into(),
        },
    };

    let x11_backend = BackendCapability {
        backend: "x11-dummy",
        usable_now: display_env_present && !x11_runtime_candidates.is_empty(),
        safe_for_daily_desktop: true,
        supports_multiple_virtual_displays: false,
        requires_free_physical_pipeline: true,
        estimated_max_virtual_displays: Some(x11_runtime_candidates.len()),
        note: if x11_runtime_candidates.is_empty() {
            "No disconnected X11 outputs available for runtime activation".into()
        } else {
            format!(
                "Session-level X11 runtime outputs available: {}",
                x11_runtime_candidates.join(", ")
            )
        },
    };

    let evdi_backend = BackendCapability {
        backend: "evdi",
        usable_now: has_dri_dir,
        safe_for_daily_desktop: !session_type.eq_ignore_ascii_case("x11"),
        supports_multiple_virtual_displays: true,
        requires_free_physical_pipeline: false,
        estimated_max_virtual_displays: Some(16),
        note: if session_type.eq_ignore_ascii_case("x11") {
            "Powerful backend, but current X11 sessions may experience disruptive hotplug behavior".into()
        } else {
            "Preferred virtual-display backend for Wayland/DRM-friendly hosts".into()
        },
    };

    let selected_backend = match choose_backend() {
        BackendChoice::Evdi => "evdi",
        BackendChoice::WaylandWlr => "wayland-wlr",
        BackendChoice::X11Dummy => "x11-dummy",
    };

    DisplayBackendInfo {
        selected_backend,
        host: HostDisplayEnvironment {
            session_type,
            display_env_present,
            has_dri_dir,
        },
        backends: vec![evdi_backend, wayland_backend, x11_backend],
    }
}

fn choose_backend() -> BackendChoice {
    match env::var("DESPLIO_DISPLAY_BACKEND")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("evdi") => BackendChoice::Evdi,
        Some("wayland-wlr") => BackendChoice::WaylandWlr,
        Some("x11-dummy") => BackendChoice::X11Dummy,
        Some("auto") | None | Some("") => auto_backend_choice(),
        Some(_) => auto_backend_choice(),
    }
}

fn auto_backend_choice() -> BackendChoice {
    let session_type = env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let display = env::var("DISPLAY").unwrap_or_default();

    if session_type.eq_ignore_ascii_case("wayland")
        && wayland_wlr::detect_wayland_environment()
            .map(|env| env.supports_portal_virtual_monitor)
            .unwrap_or(false)
    {
        BackendChoice::WaylandWlr
    } else if session_type.eq_ignore_ascii_case("x11") || !display.is_empty() {
        BackendChoice::X11Dummy
    } else {
        BackendChoice::Evdi
    }
}
