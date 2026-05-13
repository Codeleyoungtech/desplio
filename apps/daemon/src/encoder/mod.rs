use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

use thiserror::Error;

use crate::config::{EncodeConfig, ServeConfig};

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("no captured frames were available for encoding")]
    NoFrames,
    #[error("failed to create encoder output directory: {0}")]
    CreateOutputDir(#[source] io::Error),
    #[error("failed to stage preview segment output: {0}")]
    CopyPreviewArtifact(#[source] io::Error),
    #[error("ffmpeg is not installed or not available on PATH")]
    FfmpegUnavailable,
    #[error("ffmpeg does not expose the libx264 encoder")]
    MissingLibx264,
    #[error("failed to invoke ffmpeg: {0}")]
    SpawnFailed(#[source] io::Error),
    #[error("ffmpeg encoding failed: {0}")]
    EncodeFailed(String),
}

#[derive(Debug, Clone)]
pub struct EncodedPreviewArtifact {
    pub mp4_path: PathBuf,
    pub segment_path: PathBuf,
}

pub fn encode_png_to_h264_annexb(frame_path: &std::path::Path) -> Result<Vec<u8>, EncodeError> {
    ensure_ffmpeg_with_libx264()?;

    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-loop",
            "1",
            "-i",
        ])
        .arg(frame_path)
        .args([
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-level",
            "3.1",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-x264-params",
            "repeat-headers=1:annexb=1:keyint=1:min-keyint=1:scenecut=0",
            "-pix_fmt",
            "yuv420p",
            "-bf",
            "0",
            "-f",
            "h264",
            "-",
        ])
        .output()
        .map_err(EncodeError::SpawnFailed)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(EncodeError::EncodeFailed(stderr));
    }

    Ok(output.stdout)
}

pub fn encode_h264_mp4_from_pngs(
    config: &EncodeConfig,
    serve: Option<&ServeConfig>,
    frame_paths: &[PathBuf],
) -> Result<EncodedPreviewArtifact, EncodeError> {
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

    let segment_path = serve
        .map(|serve| PathBuf::from(&serve.latest_segment_path))
        .unwrap_or_else(|| output_path.clone());

    if segment_path != output_path {
        if let Some(parent) = segment_path.parent() {
            fs::create_dir_all(parent).map_err(EncodeError::CreateOutputDir)?;
        }
        fs::copy(&output_path, &segment_path).map_err(EncodeError::CopyPreviewArtifact)?;
    }

    Ok(EncodedPreviewArtifact {
        mp4_path: output_path,
        segment_path,
    })
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
