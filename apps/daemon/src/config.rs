use crate::display::DisplayConfig;

#[derive(Debug, Clone)]
pub struct Config {
    pub display: DisplayConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            display: DisplayConfig {
                width: 1280,
                height: 800,
                refresh_hz: 60,
            },
        }
    }
}
