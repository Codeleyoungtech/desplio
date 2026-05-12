use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use tiny_http::{Header, Response, Server, StatusCode};
use tracing::{debug, info, warn};

use crate::config::ServeConfig;

pub fn spawn_preview_server(
    config: &ServeConfig,
    video_path: PathBuf,
    frame_dir: PathBuf,
    shutdown: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>, io::Error> {
    let bind_addr = config.bind_addr.clone();
    let page_path = PathBuf::from(&config.page_path);
    let server = Server::http(&bind_addr).map_err(io::Error::other)?;

    let handle = thread::Builder::new()
        .name("desplio-preview-server".into())
        .spawn(move || {
            info!(bind_addr, "M3 preview server is listening");

            while !shutdown.load(Ordering::SeqCst) {
                let request = match server.recv_timeout(Duration::from_millis(250)) {
                    Ok(Some(request)) => request,
                    Ok(None) => continue,
                    Err(err) => {
                        warn!(error = %err, "preview server receive loop failed");
                        continue;
                    }
                };

                let url = request.url().to_string();
                debug!(%url, "preview server request received");

                let response = match url.as_str() {
                    "/" => serve_file(&page_path, "text/html; charset=utf-8"),
                    "/latest.mp4" | "/video.mp4" => serve_file(&video_path, "video/mp4"),
                    "/latest-frame.png" => serve_latest_frame(&frame_dir),
                    "/status.txt" => Ok(Response::from_string(format!(
                        "preview=ready\nlatest_segment={}\nlatest_frame_dir={}\n",
                        video_path.display(),
                        frame_dir.display()
                    ))
                    .with_header(text_header("text/plain; charset=utf-8"))
                    .with_header(cache_header())),
                    _ => Ok(Response::from_string("not found")
                        .with_status_code(StatusCode(404))
                        .with_header(text_header("text/plain; charset=utf-8"))
                        .with_header(cache_header())),
                };

                match response {
                    Ok(response) => {
                        if let Err(err) = request.respond(response) {
                            warn!(error = %err, %url, "failed to respond to preview request");
                        }
                    }
                    Err(err) => {
                        let response = Response::from_string(format!("internal error: {err}"))
                            .with_status_code(StatusCode(500))
                            .with_header(text_header("text/plain; charset=utf-8"));
                        let _ = request.respond(response);
                    }
                }
            }
        })?;

    Ok(handle)
}

fn serve_file(path: &PathBuf, content_type: &str) -> Result<Response<std::io::Cursor<Vec<u8>>>, io::Error> {
    let bytes = fs::read(path)?;
    Ok(Response::from_data(bytes)
        .with_header(text_header(content_type))
        .with_header(cache_header()))
}

fn serve_latest_frame(frame_dir: &PathBuf) -> Result<Response<std::io::Cursor<Vec<u8>>>, io::Error> {
    let latest = latest_frame_path(frame_dir)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no captured preview frames found"))?;
    serve_file(&latest, "image/png")
}

fn latest_frame_path(frame_dir: &PathBuf) -> Result<Option<PathBuf>, io::Error> {
    let mut latest: Option<PathBuf> = None;

    for entry in fs::read_dir(frame_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("png") {
            continue;
        }
        match latest.as_ref() {
            None => latest = Some(path),
            Some(current) => {
                if path.file_name() > current.file_name() {
                    latest = Some(path);
                }
            }
        }
    }

    Ok(latest)
}

fn text_header(value: &str) -> Header {
    Header::from_bytes("Content-Type", value).expect("valid content-type header")
}

fn cache_header() -> Header {
    Header::from_bytes("Cache-Control", "no-store, no-cache, must-revalidate")
        .expect("valid cache-control header")
}
