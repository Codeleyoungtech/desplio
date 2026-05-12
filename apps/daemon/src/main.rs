mod config;
mod capture;
mod display;
mod encoder;

use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use config::Config;
use display::EvdiBackend;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

fn main() {
    init_tracing();

    let config = Config::default();
    let hold_secs = env::var("DESPLIO_HOLD_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5);
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_for_signal = shutdown.clone();

    ctrlc::set_handler(move || {
        shutdown_for_signal.store(true, Ordering::SeqCst);
    })
    .expect("failed to install Ctrl-C handler");

    match EvdiBackend::start(config.display) {
        Ok(mut backend) => {
            match backend.capture_frames_to_png(&config.capture) {
                Ok(paths) => {
                    if paths.is_empty() {
                        info!("frame capture disabled; no PNGs were written");
                    } else {
                        info!(frames = paths.len(), "M1 frame capture verification completed");
                        if config.encode.enabled {
                            match encoder::encode_h264_mp4_from_pngs(&config.encode, &paths) {
                                Ok(output) => {
                                    info!(
                                        output = %output.display(),
                                        "M2 H.264 encoding verification completed"
                                    );
                                }
                                Err(err) => {
                                    error!(error = %err, "failed to encode M2 verification video");
                                    std::process::exit(1);
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    error!(error = %err, "failed to capture M1 verification frame(s)");
                    std::process::exit(1);
                }
            }

            if hold_secs == 0 {
                info!("desplio M0 virtual display is running; press Ctrl-C to disconnect");
                while !shutdown.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(250));
                }
            } else {
                let deadline = Instant::now() + Duration::from_secs(hold_secs);
                info!(
                    hold_secs,
                    "desplio M0 virtual display is running; it will auto-disconnect after the hold window"
                );
                while !shutdown.load(Ordering::SeqCst) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(250));
                }
            }
            info!("shutdown requested; disconnecting virtual display");
        }
        Err(err) => {
            error!(error = %err, "failed to start evdi backend");
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
