mod config;
mod capture;
mod display;
mod encoder;
mod input;
mod server;
mod webrtc;

use std::env;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use std::path::PathBuf;

use config::Config;
use config::CaptureConfig;
use display::{CaptureReadiness, DisplayBackend};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

fn main() {
    init_tracing();

    let config = Config::default();
    let backend_info = DisplayBackend::inspect(config.display);
    let keep_display_attached = env::var("DESPLIO_KEEP_DISPLAY_ATTACHED")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);
    let hold_secs = env::var("DESPLIO_HOLD_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let startup_capture_enabled = env::var("DESPLIO_STARTUP_CAPTURE")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or_else(|| !backend_info.host.session_type.eq_ignore_ascii_case("wayland"));
    let continuous_preview_requested = env::var("DESPLIO_CONTINUOUS_PREVIEW")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(true);
    let startup_capture_requested = env::var("DESPLIO_STARTUP_CAPTURE")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or_else(|| {
            !(backend_info.selected_backend == "evdi"
                && backend_info.host.session_type.eq_ignore_ascii_case("wayland"))
        });
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_for_signal = shutdown.clone();

    ctrlc::set_handler(move || {
        shutdown_for_signal.store(true, Ordering::SeqCst);
    })
    .expect("failed to install Ctrl-C handler");

    info!(
        selected_backend = backend_info.selected_backend,
        session_type = backend_info.host.session_type,
        display_env_present = backend_info.host.display_env_present,
        has_dri_dir = backend_info.host.has_dri_dir,
        "host display backend probe completed"
    );
    if backend_info.host.session_type.eq_ignore_ascii_case("wayland")
        && backend_info.selected_backend == "evdi"
    {
        info!(
            startup_capture_enabled,
            "evdi on Wayland depends on compositor support for rendering real content onto the virtual output; GNOME Wayland and KDE Plasma Wayland are the strongest targets, while Cinnamon Wayland may show black frames or sluggishness"
        );
    }
    for backend in &backend_info.backends {
        info!(
            backend = backend.backend,
            usable_now = backend.usable_now,
            safe_for_daily_desktop = backend.safe_for_daily_desktop,
            supports_multiple_virtual_displays = backend.supports_multiple_virtual_displays,
            requires_free_physical_pipeline = backend.requires_free_physical_pipeline,
            estimated_max_virtual_displays = backend.estimated_max_virtual_displays,
            note = %backend.note,
            "display backend capability"
        );
    }

    match DisplayBackend::start(config.display) {
        Ok(mut backend) => {
            if !startup_capture_enabled {
                if let Some(summary) = backend.external_monitor_summary() {
                    info!(summary = %summary, "external monitor session summary");
                }
                info!(
                    "startup capture is disabled on this Wayland session; the virtual monitor remains attached so you can validate compositor behavior without continuous capture pressure"
                );
                hold_display_until_shutdown(
                    "virtual display remains attached for Wayland validation; it will auto-disconnect after the hold window",
                    "virtual display remains attached for Wayland validation; press Ctrl-C to disconnect",
                    hold_secs,
                    shutdown.clone(),
                );
                info!("shutdown requested; disconnecting virtual display");
                return;
            }
            match backend.capture_readiness() {
                CaptureReadiness::Ready => {
                    let preview_segment_path = PathBuf::from(&config.serve.latest_segment_path);
                    let mut preview_server = None;

                    if config.serve.enabled {
                        match server::spawn_preview_server(
                            &config.serve,
                            preview_segment_path.clone(),
                            PathBuf::from(&config.serve.latest_frame_path),
                            backend.live_video_source(),
                            shutdown.clone(),
                        ) {
                            Ok(handle) => {
                                info!(
                                    url = format!("http://{}/", config.serve.bind_addr),
                                    "M3 preview receiver is ready"
                                );
                                preview_server = Some(handle);
                            }
                            Err(err) => {
                                error!(error = %err, "failed to start M3 preview server");
                                std::process::exit(1);
                            }
                        }
                    }

                    if startup_capture_requested {
                        run_capture_encode_cycle(&mut backend, &config);
                    } else {
                        info!(
                            "startup capture verification is paused on this host backend so virtual monitor attach does not immediately trigger capture/encode work"
                        );
                    }

                    let deadline = if hold_secs == 0 {
                        None
                    } else {
                        Some(Instant::now() + Duration::from_secs(hold_secs))
                    };

                    if config.serve.enabled {
                        if continuous_preview_requested {
                            info!(
                                hold_secs,
                                refresh_interval_ms = config.serve.refresh_interval_ms,
                                "M3 rolling preview is active; the daemon will keep refreshing the latest preview artifact"
                            );

                            let mut refresh_count = 0usize;
                            let live_capture = live_capture_config(&config.capture);
                            let live_capture_interval = live_capture_interval(&config);
                            loop {
                                if shutdown.load(Ordering::SeqCst) {
                                    break;
                                }
                                if deadline.is_some_and(|limit| Instant::now() >= limit) {
                                    break;
                                }

                                thread::sleep(live_capture_interval);

                                if shutdown.load(Ordering::SeqCst) {
                                    break;
                                }
                                if deadline.is_some_and(|limit| Instant::now() >= limit) {
                                    break;
                                }

                                let paths = run_capture_publish_cycle(
                                    &mut backend,
                                    &live_capture,
                                    &config.serve.latest_frame_path,
                                    false,
                                );
                                refresh_count += 1;
                                info!(
                                    refresh_count,
                                    frames = paths.len(),
                                    latest_frame = %config.serve.latest_frame_path,
                                    "M4 live preview frame refreshed"
                                );
                            }
                        } else {
                            info!(
                                "continuous preview refresh is paused on this host backend to keep the desktop responsive; the preview server will continue serving the latest artifact if one exists"
                            );
                            hold_display_until_shutdown(
                                "virtual display remains attached while preview refresh is paused for responsiveness",
                                "virtual display remains attached while preview refresh is paused; press Ctrl-C to disconnect",
                                hold_secs,
                                shutdown.clone(),
                            );
                        }

                        info!("shutdown requested; stopping preview server and disconnecting virtual display");
                    } else if keep_display_attached {
                        hold_display_until_shutdown("virtual display remains attached for debugging; it will auto-disconnect after the hold window", "virtual display remains attached for debugging; press Ctrl-C to disconnect", hold_secs, shutdown.clone());
                        info!("shutdown requested; disconnecting virtual display");
                    } else {
                        info!("keep_display_attached is false and preview is disabled, keeping display attached for hold_secs");
                        hold_daemon_without_display(hold_secs, shutdown.clone());
                        info!("shutdown requested; stopping daemon");
                    }

                    shutdown.store(true, Ordering::SeqCst);
                    if let Some(handle) = preview_server.take() {
                        let _ = handle.join();
                    }
                }
                CaptureReadiness::Pending(reason) => {
                    info!(reason = %reason, "new external monitor is active on this backend");
                    if let Some(summary) = backend.external_monitor_summary() {
                        info!(summary = %summary, "external monitor session summary");
                    }
                    hold_display_until_shutdown(
                        "virtual display remains attached while the Wayland PipeWire capture leg is being completed",
                        "virtual display remains attached; press Ctrl-C to disconnect",
                        hold_secs,
                        shutdown.clone(),
                    );
                    info!("shutdown requested; disconnecting virtual display");
                }
            }
        }
        Err(err) => {
            error!(error = %err, "failed to start display backend");
            std::process::exit(1);
        }
    }
}

fn run_capture_encode_cycle(
    backend: &mut DisplayBackend,
    config: &Config,
) -> encoder::EncodedPreviewArtifact {
    let paths = run_capture_publish_cycle(
        backend,
        &config.capture,
        &config.serve.latest_frame_path,
        true,
    );

    info!(frames = paths.len(), "M1 frame capture verification completed");

    if !config.encode.enabled {
        error!("M3 preview requires the encoder path; encoding is disabled");
        std::process::exit(1);
    }

    match encoder::encode_h264_mp4_from_pngs(
        &config.encode,
        config.serve.enabled.then_some(&config.serve),
        &paths,
    ) {
        Ok(artifact) => {
            info!(
                output = %artifact.mp4_path.display(),
                preview_segment = %artifact.segment_path.display(),
                "M2 H.264 encoding verification completed"
            );
            artifact
        }
        Err(err) => {
            error!(error = %err, "failed to encode M2 verification video");
            std::process::exit(1);
        }
    }
}

fn run_capture_publish_cycle(
    backend: &mut DisplayBackend,
    capture: &CaptureConfig,
    latest_frame_path: &str,
    fatal_errors: bool,
) -> Vec<PathBuf> {
    let paths = match backend.capture_frames_to_png(capture) {
        Ok(paths) => paths,
        Err(err) => {
            if fatal_errors {
                error!(error = %err, "failed to capture M1 verification frame(s)");
                std::process::exit(1);
            } else {
                return Vec::new();
            }
        }
    };

    if paths.is_empty() {
        if fatal_errors {
            info!("frame capture disabled; no PNGs were written");
            std::process::exit(1);
        } else {
            return Vec::new();
        }
    }

    publish_latest_frame_artifact(&paths, latest_frame_path);
    paths
}

fn live_capture_config(capture: &CaptureConfig) -> CaptureConfig {
    let mut live_capture = capture.clone();
    live_capture.frames = env::var("DESPLIO_LIVE_CAPTURE_FRAMES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    live_capture.request_interval_ms = env::var("DESPLIO_LIVE_CAPTURE_REQUEST_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    live_capture.max_wait_secs = env::var("DESPLIO_LIVE_CAPTURE_MAX_WAIT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5);
    live_capture
}

fn live_capture_interval(config: &Config) -> Duration {
    Duration::from_millis(
        env::var("DESPLIO_LIVE_CAPTURE_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(config.serve.refresh_interval_ms)
            .max(33),
    )
}

fn publish_latest_frame_artifact(frame_paths: &[PathBuf], latest_frame_path: &str) {
    let Some(source_path) = frame_paths.last() else {
        return;
    };

    let latest_frame_path = PathBuf::from(latest_frame_path);
    if let Some(parent) = latest_frame_path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            error!(error = %err, path = %parent.display(), "failed to create latest frame artifact directory");
            return;
        }
    }

    if let Err(err) = fs::copy(source_path, &latest_frame_path) {
        error!(
            error = %err,
            source = %source_path.display(),
            target = %latest_frame_path.display(),
            "failed to publish stable latest-frame artifact"
        );
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,desplio_daemon=debug"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

fn hold_display_until_shutdown(
    timed_message: &str,
    untimed_message: &str,
    hold_secs: u64,
    shutdown: Arc<AtomicBool>,
) {
    if hold_secs == 0 {
        info!(message = untimed_message);
        while !shutdown.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(250));
        }
        return;
    }

    let deadline = Instant::now() + Duration::from_secs(hold_secs);
    info!(hold_secs, message = timed_message);
    while !shutdown.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(250));
    }
}

fn hold_daemon_without_display(hold_secs: u64, shutdown: Arc<AtomicBool>) {
    if hold_secs == 0 {
        info!("virtual display has been released; press Ctrl-C to stop the daemon");
        while !shutdown.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(250));
        }
        return;
    }

    let deadline = Instant::now() + Duration::from_secs(hold_secs);
    info!(
        hold_secs,
        "virtual display has been released; the daemon will exit after the hold window"
    );
    while !shutdown.load(Ordering::SeqCst) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(250));
    }
}
