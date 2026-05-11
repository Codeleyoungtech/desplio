use crate::display::DisplayConfig;

#[derive(Debug, Clone)]
pub struct Config {
    pub capture: CaptureConfig,
    pub display: DisplayConfig,
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub frames: usize,
    pub max_wait_secs: u64,
    pub output_dir: String,
    pub request_interval_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            capture: CaptureConfig {
                frames: 3,
                max_wait_secs: 20,
                output_dir: "artifacts/m1-frames".into(),
                request_interval_ms: 500,
            },
            display: DisplayConfig {
                width: 1280,
                height: 800,
                refresh_hz: 60,
            },
        }
    }
}
