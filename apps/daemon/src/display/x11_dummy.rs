use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;
use tracing::{info, warn};

use crate::config::CaptureConfig;

use super::DisplayConfig;

#[derive(Debug)]
pub struct X11DummyBackend {
    active_output: String,
    background_helper: Option<Child>,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
}

#[derive(Debug, Error)]
pub enum X11DummyError {
    #[error("DISPLAY is not set; X11 dummy backend cannot start")]
    MissingDisplay,
    #[error("x11 runtime backend could not find a usable disconnected output (VIRTUAL, DUMMY, HDMI, DP, or VGA) to activate in this session")]
    NoDummyOutput,
    #[error("xrandr query failed: {0}")]
    QueryFailed(String),
    #[error("failed to create or activate the X11 dummy output: {0}")]
    ActivationFailed(String),
    #[error("failed to capture X11 dummy output: {0}")]
    CaptureFailed(String),
    #[error("timed out waiting for X11 dummy output to produce a non-black frame")]
    CaptureTimedOut,
    #[error("failed to write PNG frame: {0}")]
    FrameWriteFailed(#[source] io::Error),
}

impl X11DummyBackend {
    pub fn start(config: DisplayConfig) -> Result<Self, X11DummyError> {
        let display = std::env::var("DISPLAY").unwrap_or_default();
        if display.is_empty() {
            return Err(X11DummyError::MissingDisplay);
        }

        let query = xrandr_query()?;
        let primary = find_primary_output(&query).ok_or(X11DummyError::NoDummyOutput)?;
        let dummy = find_runtime_output_candidate(&query).ok_or(X11DummyError::NoDummyOutput)?;
        let mode = ensure_x11_mode(&dummy, config.width, config.height)?;

        let activate = Command::new("xrandr")
            .args([
                "--output",
                &dummy,
                "--mode",
                &mode,
                "--right-of",
                &primary,
                "--auto",
            ])
            .output()
            .map_err(io::Error::other)
            .map_err(|err| X11DummyError::ActivationFailed(err.to_string()))?;

        if !activate.status.success() {
            return Err(X11DummyError::ActivationFailed(
                String::from_utf8_lossy(&activate.stderr).trim().to_string(),
            ));
        }

        let updated = xrandr_query()?;
        let (x, y) = find_output_position(&updated, &dummy).ok_or_else(|| {
            X11DummyError::ActivationFailed(
                "dummy output was activated but its geometry could not be read back".into(),
            )
        })?;

        info!(
            output = %dummy,
            x,
            y,
            width = config.width,
            height = config.height,
            "started X11 dummy backend"
        );

        let background_helper = match spawn_background_helper(x, y, config.width, config.height) {
            Ok(child) => child,
            Err(err) => {
                warn!(error = %err, "failed to seed X11 virtual display background");
                None
            }
        };

        Ok(Self {
            active_output: dummy,
            background_helper,
            width: config.width,
            height: config.height,
            x,
            y,
        })
    }

    pub fn capture_frames_to_png(
        &mut self,
        capture: &CaptureConfig,
    ) -> Result<Vec<PathBuf>, X11DummyError> {
        if capture.frames == 0 {
            return Ok(Vec::new());
        }

        let output_dir = Path::new(&capture.output_dir);
        std::fs::create_dir_all(output_dir).map_err(X11DummyError::FrameWriteFailed)?;

        let deadline = Instant::now() + Duration::from_secs(capture.max_wait_secs);
        let mut frames = Vec::with_capacity(capture.frames);
        let mut saw_non_black = false;

        while frames.len() < capture.frames && Instant::now() < deadline {
            let path = output_dir.join(format!("frame-{:04}.png", frames.len()));
            capture_output_region(&path, self.width, self.height, self.x, self.y)?;
            let is_black = png_is_all_black(&path)?;

            info!(
                frame_index = frames.len(),
                all_black = is_black,
                path = %path.display(),
                "captured X11 dummy frame to PNG"
            );

            frames.push(path);
            if !is_black {
                saw_non_black = true;
                if frames.len() >= capture.frames.min(3) {
                    break;
                }
            }

            thread::sleep(Duration::from_millis(capture.request_interval_ms));
        }

        if frames.is_empty() || !saw_non_black {
            return Err(X11DummyError::CaptureTimedOut);
        }

        Ok(frames)
    }
}

impl Drop for X11DummyBackend {
    fn drop(&mut self) {
        if let Some(mut child) = self.background_helper.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = Command::new("xrandr")
            .args(["--output", &self.active_output, "--off"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn xrandr_query() -> Result<String, X11DummyError> {
    let query = Command::new("xrandr")
        .arg("--query")
        .output()
        .map_err(|err| X11DummyError::QueryFailed(err.to_string()))?;

    if !query.status.success() {
        return Err(X11DummyError::QueryFailed(
            String::from_utf8_lossy(&query.stderr).trim().to_string(),
        ));
    }

    Ok(String::from_utf8_lossy(&query.stdout).into_owned())
}

pub fn runtime_output_candidates() -> Result<Vec<String>, X11DummyError> {
    let query = xrandr_query()?;
    Ok(query
        .lines()
        .filter_map(|line| {
            if line.starts_with(' ') || line.is_empty() || !line.contains(" disconnected") {
                return None;
            }

            let name = line.split_whitespace().next()?;
            if name.starts_with("VIRTUAL-")
                || name.starts_with("DUMMY-")
                || name == "default"
                || name.starts_with("HDMI-")
                || name.starts_with("DP-")
                || name.starts_with("VGA-")
            {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect())
}

fn find_primary_output(query: &str) -> Option<String> {
    query.lines().find_map(|line| {
        if line.contains(" connected primary") {
            line.split_whitespace().next().map(ToOwned::to_owned)
        } else {
            None
        }
    })
}

fn find_runtime_output_candidate(_query: &str) -> Option<String> {
    runtime_output_candidates()
        .ok()
        .and_then(|candidates| candidates.into_iter().next())
}

fn find_output_position(query: &str, output_name: &str) -> Option<(u32, u32)> {
    query.lines().find_map(|line| {
        if !line.starts_with(output_name) {
            return None;
        }

        let geometry = line
            .split_whitespace()
            .find(|token| token.contains('+') && token.chars().next().is_some_and(|ch| ch.is_ascii_digit()))?;

        let plus_index = geometry.find('+')?;
        let mut coords = geometry[plus_index + 1..].split('+');
        let x = coords.next()?.parse().ok()?;
        let y = coords.next()?.parse().ok()?;
        Some((x, y))
    })
}

fn ensure_x11_mode(output: &str, width: u32, height: u32) -> Result<String, X11DummyError> {
    let mode_name = format!("desplio-{width}x{height}-60");
    let cvt = Command::new("cvt")
        .args([width.to_string(), height.to_string(), "60".into()])
        .output()
        .map_err(|err| X11DummyError::ActivationFailed(err.to_string()))?;

    if !cvt.status.success() {
        return Ok(format!("{width}x{height}"));
    }

    let stdout = String::from_utf8_lossy(&cvt.stdout);
    let Some(modeline) = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("Modeline "))
        .map(str::trim)
    else {
        return Err(X11DummyError::ActivationFailed(
            "cvt did not return a usable modeline".into(),
        ));
    };

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

    let _ = Command::new("xrandr").arg("--newmode").args(&parts).output();
    let _ = Command::new("xrandr")
        .args(["--addmode", output, &mode_name])
        .output();

    Ok(mode_name)
}

fn capture_output_region(
    path: &Path,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
) -> Result<(), X11DummyError> {
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into());
    let input = format!("{display}+{x},{y}");
    let video_size = format!("{width}x{height}");
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "x11grab",
            "-video_size",
            &video_size,
            "-i",
            &input,
            "-frames:v",
            "1",
            "-update",
            "1",
        ])
        .arg(path)
        .output()
        .map_err(|err| X11DummyError::CaptureFailed(err.to_string()))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(X11DummyError::CaptureFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

fn png_is_all_black(path: &Path) -> Result<bool, X11DummyError> {
    let image = image::open(path).map_err(|err| X11DummyError::CaptureFailed(err.to_string()))?;
    let rgba = image.to_rgba8();
    Ok(rgba.pixels().all(|pixel| pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0))
}

fn spawn_background_helper(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<Option<Child>, X11DummyError> {
    let wallpaper_uri = current_cinnamon_wallpaper_uri()?;
    let script_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("scripts")
        .join("x11_virtual_wallpaper.py");

    let child = Command::new("python3")
        .arg(script_path)
        .arg(x.to_string())
        .arg(y.to_string())
        .arg(width.to_string())
        .arg(height.to_string())
        .arg(wallpaper_uri)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| X11DummyError::ActivationFailed(err.to_string()))?;

    Ok(Some(child))
}

fn current_cinnamon_wallpaper_uri() -> Result<String, X11DummyError> {
    let output = Command::new("gsettings")
        .args(["get", "org.cinnamon.desktop.background", "picture-uri"])
        .output()
        .map_err(|err| X11DummyError::ActivationFailed(err.to_string()))?;

    if !output.status.success() {
        return Err(X11DummyError::ActivationFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().trim_matches('\'').to_string())
}
