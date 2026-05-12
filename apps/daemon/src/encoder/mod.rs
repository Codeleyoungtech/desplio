use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

use thiserror::Error;

use crate::config::EncodeConfig;

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("no captured frames were available for encoding")]
    NoFrames,
    #[error("failed to create encoder output directory: {0}")]
    CreateOutputDir(#[source] io::Error),
    #[error("ffmpeg is not installed or not available on PATH")]
    FfmpegUnavailable,
    #[error("ffmpeg does not expose the libx264 encoder")]
    MissingLibx264,
    #[error("failed to invoke ffmpeg: {0}")]
    SpawnFailed(#[source] io::Error),
    #[error("ffmpeg encoding failed: {0}")]
    EncodeFailed(String),
}

pub fn encode_h264_mp4_from_pngs(
    config: &EncodeConfig,
    frame_paths: &[PathBuf],
) -> Result<PathBuf, EncodeError> {
    if frame_paths.is_empty() {
        return Err(EncodeError::NoFrames);
    }

    ensure_ffmpeg_with_libx264()?;

    let first_frame = &frame_paths[0];
    let frame_dir = first_frame.parent().ok_or_else(|| {
        EncodeError::EncodeFailed("captured frame path did not have a parent directory".into())
    })?;

    let output_path = PathBuf::from(&config.output_path);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(EncodeError::CreateOutputDir)?;
    }

    let input_pattern = frame_dir.join("frame-%04d.png");
    let framerate = config.framerate.max(1).to_string();
    let frame_count = frame_paths.len().to_string();

    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-framerate",
            &framerate,
            "-i",
        ])
        .arg(&input_pattern)
        .args(["-frames:v", &frame_count])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-pix_fmt",
            "yuv420p",
            "-g",
            &framerate,
            "-bf",
            "0",
            "-movflags",
            "+faststart",
        ])
        .arg(&output_path)
        .output()
        .map_err(EncodeError::SpawnFailed)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(EncodeError::EncodeFailed(stderr));
    }

    Ok(output_path)
}

fn ensure_ffmpeg_with_libx264() -> Result<(), EncodeError> {
    let version = Command::new("ffmpeg")
        .args(["-hide_banner", "-version"])
        .output()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                EncodeError::FfmpegUnavailable
            } else {
                EncodeError::SpawnFailed(err)
            }
        })?;

    if !version.status.success() {
        return Err(EncodeError::FfmpegUnavailable);
    }

    let encoders = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map_err(EncodeError::SpawnFailed)?;

    if !encoders.status.success() {
        return Err(EncodeError::FfmpegUnavailable);
    }

    let stdout = String::from_utf8_lossy(&encoders.stdout);
    if stdout.contains("libx264") {
        Ok(())
    } else {
        Err(EncodeError::MissingLibx264)
    }
}
