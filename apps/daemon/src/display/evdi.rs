use std::ffi::{CStr, c_void};
use std::fs;
use std::io;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{self, NonNull};
use std::thread;
use std::time::{Duration, Instant};

use evdi_sys::{
    EVDI_MODULE_COMPATIBILITY_VERSION_MAJOR, EVDI_MODULE_COMPATIBILITY_VERSION_MINOR,
    EVDI_STATUS_AVAILABLE, EVDI_STATUS_NOT_PRESENT, EVDI_STATUS_UNRECOGNIZED, evdi_add_device,
    evdi_buffer, evdi_check_device, evdi_close, evdi_connect, evdi_device_context, evdi_disconnect,
    evdi_event_context, evdi_get_event_ready, evdi_get_lib_version, evdi_grab_pixels,
    evdi_handle_events, evdi_lib_version, evdi_mode, evdi_open, evdi_open_attached_to,
    evdi_rect, evdi_register_buffer, evdi_request_update, wrapper_evdi_set_logging, wrapper_log_cb,
};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::capture::write_xrgb8888_png;
use crate::config::CaptureConfig;

use super::edid::build_edid;

const BUFFER_ID: c_int = 1;
const MODE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
pub struct DisplayConfig {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

pub struct EvdiBackend {
    event_fd: c_int,
    handle: NonNull<evdi_device_context>,
    framebuffer: Box<[u8]>,
    damage_rects: Box<[evdi_rect]>,
    height: i32,
    pixel_format: u32,
    stride: i32,
    width: i32,
    connected: bool,
    buffer_registered: bool,
}

#[derive(Debug, Error)]
pub enum EvdiError {
    #[error("invalid display configuration: {0}")]
    InvalidConfig(String),
    #[error("no DRM card nodes found under /dev/dri")]
    NoDrmDevices,
    #[error("failed to open an evdi device; module not loaded, no node available, or access denied")]
    OpenFailed,
    #[error("evdi module is loaded with initial_device_count=0; reload it with initial_device_count=1 so /dev/dri/card0 can be created")]
    ZeroInitialDeviceCount,
    #[error("evdi userspace library {library_major}.{library_minor} is incompatible with expected module ABI {expected_major}.{expected_minor}+")]
    IncompatibleLib {
        library_major: i32,
        library_minor: i32,
        expected_major: u32,
        expected_minor: u32,
    },
    #[error("timed out waiting for compositor mode negotiation")]
    ModeWaitTimedOut,
    #[error("poll on evdi event fd failed: {0}")]
    PollFailed(#[source] io::Error),
    #[error("event fd was invalid")]
    InvalidEventFd,
    #[error("failed to derive card number from {0}")]
    InvalidCardName(String),
    #[error("mode negotiated by compositor was invalid: {0}")]
    InvalidNegotiatedMode(String),
    #[error("M0 currently supports only 1280x800@60 for the validated EDID path; requested {width}x{height}@{refresh_hz}")]
    UnsupportedMode {
        width: u32,
        height: u32,
        refresh_hz: u32,
    },
    #[error("timed out waiting for evdi frame updates")]
    FrameCaptureTimedOut,
    #[error("failed to write frame image: {0}")]
    FrameWriteFailed(#[source] io::Error),
    #[error("unsupported pixel format for PNG export: {0}")]
    UnsupportedPixelFormat(u32),
}

#[derive(Debug, Clone, Copy)]
struct NegotiatedMode {
    width: i32,
    height: i32,
    refresh_rate: i32,
    bits_per_pixel: i32,
    pixel_format: u32,
}

#[derive(Default)]
struct EventState {
    latest_mode: Option<NegotiatedMode>,
    last_dpms: Option<i32>,
}

#[derive(Default)]
struct CaptureEventState {
    update_ready: bool,
}

impl EvdiBackend {
    pub fn start(config: DisplayConfig) -> Result<Self, EvdiError> {
        validate_config(config)?;
        ensure_drm_devices_exist()?;
        register_logging();
        log_library_version()?;

        let handle = open_handle()?;
        info!("evdi handle opened");

        let edid = build_edid(config)?;
        unsafe {
            evdi_connect(
                handle.as_ptr(),
                edid.as_ptr(),
                edid.len() as u32,
                config.width * config.height,
            );
        }
        info!(
            width = config.width,
            height = config.height,
            refresh_hz = config.refresh_hz,
            "EDID connected to evdi device"
        );

        let negotiated = wait_for_mode_change(handle)?;
        info!(
            width = negotiated.width,
            height = negotiated.height,
            refresh_hz = negotiated.refresh_rate,
            bits_per_pixel = negotiated.bits_per_pixel,
            pixel_format = negotiated.pixel_format,
            "mode_changed received from compositor"
        );

        let event_fd = unsafe { evdi_get_event_ready(handle.as_ptr()) };
        if event_fd < 0 {
            return Err(EvdiError::InvalidEventFd);
        }

        let stride = compute_stride(negotiated)?;
        let buffer_len = stride as usize * negotiated.height as usize;
        let mut framebuffer = vec![0u8; buffer_len].into_boxed_slice();
        let mut damage_rects = vec![
            evdi_rect {
                x1: 0,
                y1: 0,
                x2: negotiated.width,
                y2: negotiated.height,
            };
            16
        ]
        .into_boxed_slice();

        unsafe {
            evdi_register_buffer(
                handle.as_ptr(),
                evdi_buffer {
                    id: BUFFER_ID,
                    buffer: framebuffer.as_mut_ptr() as *mut c_void,
                    width: negotiated.width,
                    height: negotiated.height,
                    stride,
                    rects: damage_rects.as_mut_ptr(),
                    rect_count: 0,
                },
            );
        }

        info!(
            width = negotiated.width,
            height = negotiated.height,
            stride,
            "evdi buffer registered; virtual monitor should now appear as a real display"
        );

        Ok(Self {
            event_fd,
            handle,
            framebuffer,
            damage_rects,
            height: negotiated.height,
            pixel_format: negotiated.pixel_format,
            stride,
            width: negotiated.width,
            connected: true,
            buffer_registered: true,
        })
    }

    pub fn capture_frames_to_png(
        &mut self,
        capture: &CaptureConfig,
    ) -> Result<Vec<PathBuf>, EvdiError> {
        if capture.frames == 0 {
            return Ok(Vec::new());
        }

        if self.pixel_format != 875_713_112 {
            return Err(EvdiError::UnsupportedPixelFormat(self.pixel_format));
        }

        let deadline = Instant::now() + Duration::from_secs(capture.max_wait_secs);
        let mut frames = Vec::with_capacity(capture.frames);
        let output_dir = Path::new(&capture.output_dir);
        let mut attempted_x11_activation = false;
        let mut capture_state = CaptureEventState::default();
        let mut event_ctx = evdi_event_context {
            dpms_handler: Some(dpms_handler),
            mode_changed_handler: Some(mode_changed_handler),
            update_ready_handler: Some(update_ready_handler),
            crtc_state_handler: None,
            cursor_set_handler: None,
            cursor_move_handler: None,
            ddcci_data_handler: None,
            user_data: (&mut capture_state as *mut CaptureEventState).cast::<c_void>(),
        };

        while frames.len() < capture.frames && Instant::now() < deadline {
            capture_state.update_ready = false;
            let requested = unsafe { evdi_request_update(self.handle.as_ptr(), BUFFER_ID) };
            debug!(requested, "requested evdi frame update");
            if !requested {
                if !attempted_x11_activation {
                    attempted_x11_activation = true;
                    match try_activate_x11_output(self.width as u32, self.height as u32) {
                        Ok(X11ActivationOutcome::Activated) => {
                            warn!("evdi did not accept the first frame request; attempted X11 output activation to encourage scanout");
                        }
                        Ok(X11ActivationOutcome::NoCandidate) => {
                            warn!("evdi did not accept the first frame request; no X11 output activation candidate was found");
                        }
                        Ok(X11ActivationOutcome::Failed(message)) => {
                            warn!(error = %message, "evdi did not accept the first frame request; X11 output activation failed");
                        }
                        Err(err) => {
                            warn!(error = %err, "evdi did not accept the first frame request; X11 output activation assist failed");
                        }
                    }
                }
                if poll_event_fd(self.event_fd, Duration::from_millis(capture.request_interval_ms))? {
                    unsafe {
                        evdi_handle_events(self.handle.as_ptr(), &mut event_ctx);
                    }
                }
                continue;
            }

            let frame_deadline = Instant::now() + Duration::from_millis(capture.request_interval_ms.max(1000));
            while Instant::now() < frame_deadline && !capture_state.update_ready {
                if poll_event_fd(self.event_fd, Duration::from_millis(100))? {
                    unsafe {
                        evdi_handle_events(self.handle.as_ptr(), &mut event_ctx);
                    }
                }
            }

            if capture_state.update_ready {
                let mut rect_count = self.damage_rects.len() as c_int;
                unsafe {
                    evdi_grab_pixels(
                        self.handle.as_ptr(),
                        self.damage_rects.as_mut_ptr(),
                        &mut rect_count,
                    );
                }

                let path = write_xrgb8888_png(
                    output_dir,
                    frames.len(),
                    self.width as usize,
                    self.height as usize,
                    self.stride as usize,
                    &self.framebuffer,
                )
                .map_err(EvdiError::FrameWriteFailed)?;

                let is_black =
                    framebuffer_is_all_black(&self.framebuffer, self.width as usize, self.height as usize, self.stride as usize);

                info!(
                    frame_index = frames.len(),
                    all_black = is_black,
                    dirty_rects = rect_count,
                    path = %path.display(),
                    "captured evdi frame to PNG"
                );
                frames.push(path);

                if is_black && !attempted_x11_activation {
                    attempted_x11_activation = true;
                    match try_activate_x11_output(self.width as u32, self.height as u32) {
                        Ok(X11ActivationOutcome::Activated) => {
                            warn!("captured frame was fully black; attempted X11 output activation to encourage real compositor content");
                        }
                        Ok(X11ActivationOutcome::NoCandidate) => {
                            warn!("captured frame was fully black; no X11 output activation candidate was found");
                        }
                        Ok(X11ActivationOutcome::Failed(message)) => {
                            warn!(error = %message, "captured frame was fully black; X11 output activation failed");
                        }
                        Err(err) => {
                            warn!(error = %err, "captured frame was fully black; X11 output activation assist failed");
                        }
                    }
                }
            } else {
                if !attempted_x11_activation {
                    attempted_x11_activation = true;
                    match try_activate_x11_output(self.width as u32, self.height as u32) {
                        Ok(X11ActivationOutcome::Activated) => {
                            warn!("timed out waiting for evdi update_ready; attempted X11 output activation to encourage scanout");
                        }
                        Ok(X11ActivationOutcome::NoCandidate) => {
                            warn!("timed out waiting for evdi update_ready; no X11 output activation candidate was found");
                        }
                        Ok(X11ActivationOutcome::Failed(message)) => {
                            warn!(error = %message, "timed out waiting for evdi update_ready; X11 output activation failed");
                        }
                        Err(err) => {
                            warn!(error = %err, "timed out waiting for evdi update_ready; X11 output activation assist failed");
                        }
                    }
                }
                debug!("timed out waiting for evdi update_ready after requesting a frame");
            }
        }

        if frames.is_empty() {
            return Err(EvdiError::FrameCaptureTimedOut);
        }

        Ok(frames)
    }
}

unsafe extern "C" fn update_ready_handler(_buffer_to_be_updated: c_int, user_data: *mut c_void) {
    let state = &mut *(user_data as *mut CaptureEventState);
    state.update_ready = true;
}

fn framebuffer_is_all_black(framebuffer: &[u8], width: usize, height: usize, stride: usize) -> bool {
    for y in 0..height {
        let row = &framebuffer[y * stride..y * stride + width * 4];
        for px in row.chunks_exact(4) {
            if px[0] != 0 || px[1] != 0 || px[2] != 0 {
                return false;
            }
        }
    }
    true
}

enum X11ActivationOutcome {
    Activated,
    Failed(String),
    NoCandidate,
}

fn try_activate_x11_output(width: u32, height: u32) -> Result<X11ActivationOutcome, io::Error> {
    if std::env::var("DISPLAY").ok().filter(|value| !value.is_empty()).is_none() {
        return Ok(X11ActivationOutcome::NoCandidate);
    }

    let _ = try_link_x11_providers()?;

    let query = Command::new("xrandr").arg("--query").output()?;
    if !query.status.success() {
        return Ok(X11ActivationOutcome::Failed(
            String::from_utf8_lossy(&query.stderr).trim().to_string(),
        ));
    }

    let stdout = String::from_utf8_lossy(&query.stdout);
    let mut primary_output: Option<String> = None;
    let mut first_connected: Option<String> = None;
    let mut candidate_output: Option<String> = None;

    for line in stdout.lines() {
        if line.starts_with(' ') || line.is_empty() {
            continue;
        }

        let name = line.split_whitespace().next().unwrap_or_default().to_string();
        if line.contains(" connected primary") {
            primary_output = Some(name.clone());
        }
        if first_connected.is_none() && line.contains(" connected ") {
            first_connected = Some(name.clone());
        }
        if candidate_output.is_none()
            && line.contains(" disconnected")
            && (name.starts_with("DVI-I-")
                || name.starts_with("DVI-")
                || name.starts_with("VIRTUAL-")
                || name.starts_with("DP-")
                || name.starts_with("HDMI-"))
        {
            candidate_output = Some(name);
        }
    }

    let primary = primary_output.or(first_connected);
    let candidate = candidate_output;

    let (primary, candidate) = match (primary, candidate) {
        (Some(primary), Some(candidate)) => (primary, candidate),
        _ => return Ok(X11ActivationOutcome::NoCandidate),
    };

    let mode = ensure_x11_mode(&candidate, width, height)?;

    let status = Command::new("xrandr")
        .args([
            "--output",
            &candidate,
            "--mode",
            &mode,
            "--right-of",
            &primary,
            "--auto",
        ])
        .output()?;

    if status.status.success() {
        Ok(X11ActivationOutcome::Activated)
    } else {
        Ok(X11ActivationOutcome::Failed(
            String::from_utf8_lossy(&status.stderr).trim().to_string(),
        ))
    }
}

fn ensure_x11_mode(output: &str, width: u32, height: u32) -> Result<String, io::Error> {
    let mode_name = format!("desplio-{width}x{height}-60");
    let cvt = Command::new("cvt")
        .args([width.to_string(), height.to_string(), "60".into()])
        .output()?;

    if !cvt.status.success() {
        return Ok(format!("{width}x{height}"));
    }

    let stdout = String::from_utf8_lossy(&cvt.stdout);
    let modeline = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("Modeline "))
        .map(str::trim)
        .ok_or_else(|| io::Error::other("cvt did not return a Modeline"))?;

    let parts: Vec<String> = modeline
        .split_whitespace()
        .skip(1)
        .map(|part| {
            if part.starts_with('"') && part.ends_with('"') {
                mode_name.clone()
            } else {
                part.to_string()
            }
        })
        .collect();

    let _ = Command::new("xrandr")
        .arg("--newmode")
        .args(&parts)
        .output()?;

    let addmode = Command::new("xrandr")
        .args(["--addmode", output, &mode_name])
        .output()?;

    if !addmode.status.success() {
        let stderr = String::from_utf8_lossy(&addmode.stderr);
        if !stderr.contains("already exists") && !stderr.contains("cannot find mode") {
            return Err(io::Error::other(stderr.trim().to_string()));
        }
    }

    Ok(mode_name)
}

fn try_link_x11_providers() -> Result<bool, io::Error> {
    let providers = Command::new("xrandr").arg("--listproviders").output()?;
    if !providers.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&providers.stdout);
    let mut source_provider: Option<String> = None;
    let mut sink_provider: Option<String> = None;

    for line in stdout.lines() {
        if !line.trim_start().starts_with("Provider ") {
            continue;
        }

        if let Some(id) = provider_id_from_line(line) {
            if source_provider.is_none() && line.contains("Source Output") {
                source_provider = Some(id.clone());
            }
            if sink_provider.is_none() && line.contains("Sink Output") && !line.contains("Source Output") {
                sink_provider = Some(id);
            }
        }
    }

    let (sink, source) = match (sink_provider, source_provider) {
        (Some(sink), Some(source)) => (sink, source),
        _ => return Ok(false),
    };

    let output = Command::new("xrandr")
        .args(["--setprovideroutputsource", &sink, &source])
        .output()?;

    Ok(output.status.success())
}

fn provider_id_from_line(line: &str) -> Option<String> {
    let marker = "id:";
    let idx = line.find(marker)?;
    let rest = &line[idx + marker.len()..];
    let id = rest.split_whitespace().next()?;
    Some(id.to_string())
}

impl Drop for EvdiBackend {
    fn drop(&mut self) {
        unsafe {
            if self.connected {
                if self.buffer_registered {
                    debug!("disconnecting evdi device");
                }
                evdi_disconnect(self.handle.as_ptr());
            }
            evdi_close(self.handle.as_ptr());
        }
    }
}

fn validate_config(config: DisplayConfig) -> Result<(), EvdiError> {
    if config.width == 0 || config.height == 0 {
        return Err(EvdiError::InvalidConfig(
            "width and height must be greater than zero".into(),
        ));
    }

    if config.refresh_hz == 0 {
        return Err(EvdiError::InvalidConfig(
            "refresh_hz must be greater than zero".into(),
        ));
    }

    Ok(())
}

fn ensure_drm_devices_exist() -> Result<(), EvdiError> {
    let dri = Path::new("/dev/dri");
    let entries = fs::read_dir(dri).map_err(|_| EvdiError::NoDrmDevices)?;
    let has_card = entries
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("card"));

    if has_card {
        Ok(())
    } else {
        Err(EvdiError::NoDrmDevices)
    }
}

fn log_library_version() -> Result<(), EvdiError> {
    let mut version = evdi_lib_version {
        version_major: 0,
        version_minor: 0,
        version_patchlevel: 0,
    };

    unsafe { evdi_get_lib_version(&mut version) };

    info!(
        major = version.version_major,
        minor = version.version_minor,
        patch = version.version_patchlevel,
        "loaded libevdi compatibility layer"
    );

    if version.version_major != EVDI_MODULE_COMPATIBILITY_VERSION_MAJOR as i32
        || version.version_minor < EVDI_MODULE_COMPATIBILITY_VERSION_MINOR as i32
    {
        return Err(EvdiError::IncompatibleLib {
            library_major: version.version_major,
            library_minor: version.version_minor,
            expected_major: EVDI_MODULE_COMPATIBILITY_VERSION_MAJOR,
            expected_minor: EVDI_MODULE_COMPATIBILITY_VERSION_MINOR,
        });
    }

    Ok(())
}

fn open_handle() -> Result<NonNull<evdi_device_context>, EvdiError> {
    if evdi_initial_device_count_is_zero() {
        return Err(EvdiError::ZeroInitialDeviceCount);
    }

    let attached = unsafe { evdi_open_attached_to(ptr::null()) };
    if let Some(handle) = NonNull::new(attached) {
        info!("opened evdi handle via evdi_open_attached_to");
        return Ok(handle);
    }

    let added_device = unsafe { evdi_add_device() };
    if added_device >= 0 {
        info!(card_number = added_device, "requested creation of a new evdi device");
        let handle = unsafe { evdi_open(added_device) };
        if let Some(handle) = NonNull::new(handle) {
            info!(card_number = added_device, "opened evdi handle via evdi_add_device fallback");
            return Ok(handle);
        }
        warn!(
            card_number = added_device,
            "evdi device was added but opening its DRM node still failed"
        );
    } else {
        debug!("evdi_add_device did not create a new DRM node");
    }

    for card_number in available_card_numbers()? {
        let status = unsafe { evdi_check_device(card_number) };
        debug!(card_number, status, "inspected DRM card while probing for evdi");
        if status == EVDI_STATUS_NOT_PRESENT {
            continue;
        }
        if status == EVDI_STATUS_UNRECOGNIZED {
            warn!(card_number, "found DRM card that is not recognized as evdi");
            continue;
        }
        if status == EVDI_STATUS_AVAILABLE {
            let handle = unsafe { evdi_open(card_number) };
            if let Some(handle) = NonNull::new(handle) {
                return Ok(handle);
            }
        }
    }

    Err(EvdiError::OpenFailed)
}

fn evdi_initial_device_count_is_zero() -> bool {
    match fs::read_to_string("/sys/module/evdi/parameters/initial_device_count") {
        Ok(value) => value.trim() == "0",
        Err(_) => false,
    }
}

fn available_card_numbers() -> Result<Vec<c_int>, EvdiError> {
    let mut cards = Vec::new();
    for entry in fs::read_dir("/dev/dri").map_err(|_| EvdiError::NoDrmDevices)? {
        let entry = entry.map_err(|_| EvdiError::NoDrmDevices)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(suffix) = name.strip_prefix("card") {
            let number = suffix
                .parse::<c_int>()
                .map_err(|_| EvdiError::InvalidCardName(name.to_string()))?;
            cards.push(number);
        }
    }
    cards.sort_unstable();
    Ok(cards)
}

fn wait_for_mode_change(
    handle: NonNull<evdi_device_context>,
) -> Result<NegotiatedMode, EvdiError> {
    let fd = unsafe { evdi_get_event_ready(handle.as_ptr()) };
    if fd < 0 {
        return Err(EvdiError::InvalidEventFd);
    }

    let mut state = EventState::default();
    let mut ctx = evdi_event_context {
        dpms_handler: Some(dpms_handler),
        mode_changed_handler: Some(mode_changed_handler),
        update_ready_handler: None,
        crtc_state_handler: None,
        cursor_set_handler: None,
        cursor_move_handler: None,
        ddcci_data_handler: None,
        user_data: (&mut state as *mut EventState).cast::<c_void>(),
    };

    let start = Instant::now();
    while start.elapsed() < MODE_WAIT_TIMEOUT {
        if poll_event_fd(fd, Duration::from_millis(250))? {
            unsafe {
                evdi_handle_events(handle.as_ptr(), &mut ctx);
            }
        }
        if let Some(mode) = state.latest_mode {
            if let Some(dpms) = state.last_dpms {
                debug!(dpms_mode = dpms, "received DPMS state while waiting for mode");
            }
            return Ok(mode);
        }
        thread::sleep(Duration::from_millis(50));
    }

    log_evdi_connector_statuses();
    Err(EvdiError::ModeWaitTimedOut)
}

fn poll_event_fd(fd: c_int, timeout: Duration) -> Result<bool, EvdiError> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };

    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as c_int;
    let ret = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
    if ret < 0 {
        return Err(EvdiError::PollFailed(io::Error::last_os_error()));
    }

    Ok(ret > 0 && (poll_fd.revents & libc::POLLIN) != 0)
}

fn log_evdi_connector_statuses() {
    let entries = match fs::read_dir("/sys/class/drm") {
        Ok(entries) => entries,
        Err(err) => {
            warn!(error = %err, "failed to inspect DRM connector state after mode wait timeout");
            return;
        }
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card0-") {
            continue;
        }

        let status_path = entry.path().join("status");
        match fs::read_to_string(&status_path) {
            Ok(status) => {
                warn!(
                    connector = name,
                    status = status.trim(),
                    "evdi connector state after mode negotiation timeout"
                );
            }
            Err(err) => {
                warn!(
                    connector = name,
                    error = %err,
                    "failed to read evdi connector status after timeout"
                );
            }
        }
    }
}

fn compute_stride(mode: NegotiatedMode) -> Result<c_int, EvdiError> {
    if mode.width <= 0 || mode.height <= 0 {
        return Err(EvdiError::InvalidNegotiatedMode(
            "width and height must be positive".into(),
        ));
    }

    let bytes_per_pixel = if mode.bits_per_pixel >= 32 {
        4
    } else if mode.bits_per_pixel >= 24 {
        4
    } else {
        return Err(EvdiError::InvalidNegotiatedMode(format!(
            "unsupported bits_per_pixel {}",
            mode.bits_per_pixel
        )));
    };

    Ok(mode.width.saturating_mul(bytes_per_pixel))
}

unsafe extern "C" fn dpms_handler(dpms_mode: c_int, user_data: *mut c_void) {
    let state = &mut *(user_data as *mut EventState);
    state.last_dpms = Some(dpms_mode);
    debug!(dpms_mode, "evdi DPMS callback received");
}

unsafe extern "C" fn mode_changed_handler(mode: evdi_mode, user_data: *mut c_void) {
    let state = &mut *(user_data as *mut EventState);
    state.latest_mode = Some(NegotiatedMode {
        width: mode.width,
        height: mode.height,
        refresh_rate: mode.refresh_rate,
        bits_per_pixel: mode.bits_per_pixel,
        pixel_format: mode.pixel_format,
    });
}

unsafe extern "C" fn evdi_log_callback(user_data: *mut c_void, msg: *const c_char) {
    let _ = user_data;
    if msg.is_null() {
        return;
    }

    if let Ok(message) = CStr::from_ptr(msg).to_str() {
        debug!(target: "libevdi", "{message}");
    }
}

fn register_logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        wrapper_evdi_set_logging(wrapper_log_cb {
            function: Some(evdi_log_callback),
            user_data: ptr::null_mut(),
        });
    });
}
