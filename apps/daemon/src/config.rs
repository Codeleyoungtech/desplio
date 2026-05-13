use crate::display::DisplayConfig;

#[derive(Debug, Clone)]
pub struct Config {
    pub capture: CaptureConfig,
    pub display: DisplayConfig,
    pub encode: EncodeConfig,
    pub serve: ServeConfig,
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub frames: usize,
    pub max_wait_secs: u64,
    pub output_dir: String,
    pub request_interval_ms: u64,
}

#[derive(Debug, Clone)]
pub struct EncodeConfig {
    pub enabled: bool,
    pub output_path: String,
    pub framerate: u32,
}

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub enabled: bool,
    pub bind_addr: String,
    pub page_path: String,
    pub latest_segment_path: String,
    pub latest_frame_path: String,
    pub refresh_interval_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            capture: CaptureConfig {
                frames: 10,
                max_wait_secs: 20,
                output_dir: "artifacts/m1-frames".into(),
                request_interval_ms: 1000,
            },
            display: DisplayConfig {
                width: 1280,
                height: 800,
                refresh_hz: 60,
            },
            encode: EncodeConfig {
                enabled: true,
                output_path: "artifacts/m2-video/desplio-m2.mp4".into(),
                framerate: 1,
            },
            serve: ServeConfig {
                enabled: true,
                bind_addr: "127.0.0.1:9001".into(),
                page_path: "apps/web-client/index.html".into(),
                latest_segment_path: "artifacts/m3-preview/latest.mp4".into(),
                latest_frame_path: "artifacts/m3-preview/latest-frame.png".into(),
                refresh_interval_ms: 1500,
            },
        }
    }
}
