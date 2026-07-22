use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use astera_config::{Config, ConfigError};

const RELOAD_DEBOUNCE: Duration = Duration::from_millis(200);

pub(super) struct ConfigWatcher {
    path: PathBuf,
    stamp: Option<SystemTime>,
    exists: bool,
    reload_at: Option<Instant>,
}

impl ConfigWatcher {
    pub(super) fn new(path: PathBuf) -> Self {
        let metadata = std::fs::metadata(&path).ok();
        Self {
            path,
            stamp: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            exists: metadata.is_some(),
            reload_at: None,
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn poll(&mut self, now: Instant) -> Option<Result<Config, ConfigError>> {
        let metadata = std::fs::metadata(&self.path).ok();
        let exists = metadata.is_some();
        let stamp = metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok());
        if exists != self.exists || stamp != self.stamp {
            self.exists = exists;
            self.stamp = stamp;
            self.reload_at = Some(now + RELOAD_DEBOUNCE);
        }
        if !self.reload_at.is_some_and(|deadline| now >= deadline) {
            return None;
        }
        self.reload_at = None;
        Some(if self.exists {
            Config::load(&self.path)
        } else {
            Ok(Config::default())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletion_restores_built_in_configuration_after_debounce() {
        let path = std::env::temp_dir().join(format!(
            "astera-config-watcher-{}-{}.ron",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "(bindings: {})").unwrap();
        let mut watcher = ConfigWatcher::new(path.clone());
        std::fs::remove_file(&path).unwrap();
        let now = Instant::now();
        assert!(watcher.poll(now).is_none());
        let config = watcher.poll(now + RELOAD_DEBOUNCE).unwrap().unwrap();
        assert!(!config.bindings.is_empty());
    }
}
