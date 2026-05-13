mod config;
mod capture;
mod display;
mod encoder;
mod server;
mod webrtc;

use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use config::Config;
use display::DisplayBackend;
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
        .unwrap_or(if config.serve.enabled { 0 } else { 5 });
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
            let initial_artifact = run_capture_encode_cycle(&mut backend, &config);
            let mut preview_server = None;

            if config.serve.enabled {
                match server::spawn_preview_server(
                    &config.serve,
                    initial_artifact.segment_path.clone(),
                    config.capture.output_dir.clone().into(),
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

            let deadline = if hold_secs == 0 {
                None
            } else {
                Some(Instant::now() + Duration::from_secs(hold_secs))
            };

            if config.serve.enabled {
                info!(
                    hold_secs,
                    refresh_interval_ms = config.serve.refresh_interval_ms,
                    "M3 rolling preview is active; the daemon will keep refreshing the latest preview artifact"
                );

                let mut refresh_count = 0usize;
                loop {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    if deadline.is_some_and(|limit| Instant::now() >= limit) {
                        break;
                    }

                    thread::sleep(Duration::from_millis(config.serve.refresh_interval_ms));

                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    if deadline.is_some_and(|limit| Instant::now() >= limit) {
                        break;
                    }

                    let artifact = run_capture_encode_cycle(&mut backend, &config);
                    refresh_count += 1;
                    info!(
                        refresh_count,
                        output = %artifact.segment_path.display(),
                        "M3 rolling preview artifact refreshed"
                    );
                }

                info!("shutdown requested; stopping preview server and disconnecting virtual display");
            } else if keep_display_attached {
                if let Some(deadline) = deadline {
                    info!(
                        hold_secs,
                        "virtual display remains attached for debugging; it will auto-disconnect after the hold window"
                    );
                    while !shutdown.load(Ordering::SeqCst) && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(250));
                    }
                } else {
                    info!("virtual display remains attached for debugging; press Ctrl-C to disconnect");
                    while !shutdown.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(250));
                    }
                }
                info!("shutdown requested; disconnecting virtual display");
            } else {
                info!("releasing virtual display immediately after capture to keep the desktop usable");
                drop(backend);
                if let Some(deadline) = deadline {
                    info!(
                        hold_secs,
                        "virtual display has been released; the daemon will exit after the hold window"
                    );
                    while !shutdown.load(Ordering::SeqCst) && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(250));
                    }
                } else {
                    info!("virtual display has been released; press Ctrl-C to stop the daemon");
                    while !shutdown.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(250));
                    }
                }
                info!("shutdown requested; stopping daemon");
            }

            shutdown.store(true, Ordering::SeqCst);
            if let Some(handle) = preview_server.take() {
                let _ = handle.join();
            }
        }
        Err(err) => {
            error!(error = %err, "failed to start evdi backend");
            std::process::exit(1);
        }
    }
}

fn run_capture_encode_cycle(
    backend: &mut DisplayBackend,
    config: &Config,
) -> encoder::EncodedPreviewArtifact {
    let paths = match backend.capture_frames_to_png(&config.capture) {
        Ok(paths) => paths,
        Err(err) => {
            error!(error = %err, "failed to capture M1 verification frame(s)");
            std::process::exit(1);
        }
    };

    if paths.is_empty() {
        info!("frame capture disabled; no PNGs were written");
        std::process::exit(1);
    }

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

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,desplio_daemon=debug"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
