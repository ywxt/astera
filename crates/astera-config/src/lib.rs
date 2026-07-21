use std::{fs, path::Path, time::Duration};

use astera_core::CameraPolicy;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub gap: i64,
    pub animation_ms: u64,
    pub camera: CameraPolicy,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gap: 8,
            animation_ms: 280,
            camera: CameraPolicy::KeepVisible { margin: 32 },
        }
    }
}

impl Config {
    pub fn animation_duration(&self) -> Duration {
        Duration::from_millis(self.animation_ms)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path)?;
        let config: Self = ron::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.gap < 0 {
            return Err(ConfigError::Invalid("gap cannot be negative"));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse RON configuration: {0}")]
    Parse(#[from] ron::error::SpannedError),
    #[error("invalid configuration: {0}")]
    Invalid(&'static str),
}
