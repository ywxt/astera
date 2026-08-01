use std::{
    io,
    mem::MaybeUninit,
    os::fd::AsFd,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use astera_config::{Config, ConfigError};
use rustix::{
    fd::OwnedFd,
    fs::inotify::{self, CreateFlags, WatchFlags},
};

const RELOAD_DEBOUNCE: Duration = Duration::from_millis(200);

pub(super) struct ConfigWatcher {
    /// Target file, rather than an open descriptor, so atomic rename saves are observed.
    path: PathBuf,
    exists: bool,
    stamp: Option<SystemTime>,
    /// Deferred reload deadline that coalesces multi-step editor saves.
    reload_at: Option<Instant>,
    inotify: OwnedFd,
    watched_directory: PathBuf,
    event_driven: bool,
}

impl ConfigWatcher {
    pub(super) fn new(mut path: PathBuf) -> io::Result<Self> {
        if path.is_relative() {
            path = std::env::current_dir()?.join(path);
        }
        let metadata = std::fs::metadata(&path).ok();
        let inotify = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        let watched_directory = nearest_existing_directory(&path);
        inotify::add_watch(
            &inotify,
            &watched_directory,
            WatchFlags::ATTRIB
                | WatchFlags::CLOSE_WRITE
                | WatchFlags::CREATE
                | WatchFlags::DELETE
                | WatchFlags::DELETE_SELF
                | WatchFlags::MOVE
                | WatchFlags::MOVE_SELF,
        )?;
        Ok(Self {
            path,
            exists: metadata.is_some(),
            stamp: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            reload_at: None,
            inotify,
            watched_directory,
            event_driven: false,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn exists(&self) -> bool {
        self.exists
    }

    pub(super) fn duplicate_fd(&mut self) -> io::Result<OwnedFd> {
        self.event_driven = true;
        Ok(rustix::io::dup(self.inotify.as_fd())?)
    }

    pub(super) fn notify(&mut self, now: Instant) -> io::Result<()> {
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.inotify, &mut buffer);
        let mut changed = false;
        let mut rearm = false;
        loop {
            match reader.next() {
                Ok(event) => {
                    let events = event.events();
                    rearm |= events.intersects(
                        inotify::ReadFlags::DELETE_SELF
                            | inotify::ReadFlags::IGNORED
                            | inotify::ReadFlags::MOVE_SELF,
                    );
                    changed |= rearm || self.event_is_relevant(event.file_name());
                }
                Err(error) if error == rustix::io::Errno::AGAIN => break,
                Err(error) => return Err(error.into()),
            }
        }
        if changed {
            // Any ancestor event may make a closer directory watchable after an atomic save or
            // first-time config-directory creation.
            let directory = nearest_existing_directory(&self.path);
            if rearm || directory != self.watched_directory {
                inotify::add_watch(
                    &self.inotify,
                    &directory,
                    WatchFlags::ATTRIB
                        | WatchFlags::CLOSE_WRITE
                        | WatchFlags::CREATE
                        | WatchFlags::DELETE
                        | WatchFlags::DELETE_SELF
                        | WatchFlags::MOVE
                        | WatchFlags::MOVE_SELF,
                )?;
                self.watched_directory = directory;
            }
            self.exists = self.path.is_file();
            self.stamp = std::fs::metadata(&self.path)
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            self.reload_at = Some(now + RELOAD_DEBOUNCE);
        }
        Ok(())
    }

    pub(super) fn poll(&mut self, now: Instant) -> Option<Result<Config, ConfigError>> {
        if !self.event_driven {
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
        }
        if !self.reload_at.is_some_and(|deadline| now >= deadline) {
            return None;
        }
        self.reload_at = None;
        // A deleted implicit config returns to defaults; parse errors are returned to the caller,
        // which keeps the last valid runtime configuration active.
        Some(if self.exists {
            Config::load(&self.path)
        } else {
            Ok(Config::default())
        })
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.reload_at
    }

    fn event_is_relevant(&self, name: Option<&std::ffi::CStr>) -> bool {
        let Some(name) = name else {
            return true;
        };
        let Ok(relative) = self.path.strip_prefix(&self.watched_directory) else {
            return true;
        };
        let Some(component) = relative.components().next() else {
            return true;
        };
        component.as_os_str().as_bytes() == name.to_bytes()
    }
}

fn nearest_existing_directory(path: &Path) -> PathBuf {
    let mut candidate = path.parent().unwrap_or_else(|| Path::new("/"));
    while !candidate.is_dir() {
        candidate = candidate.parent().unwrap_or_else(|| Path::new("/"));
    }
    candidate.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn temporary_directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "astera-config-watch-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    fn notify_until_changed(watcher: &mut ConfigWatcher, now: Instant) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            watcher.notify(now).unwrap();
            if watcher.reload_at.is_some() {
                return;
            }
            std::thread::yield_now();
        }
        panic!("timed out waiting for inotify event");
    }

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
        std::fs::write(&path, "").unwrap();
        let mut watcher = ConfigWatcher::new(path.clone()).unwrap();
        std::fs::remove_file(&path).unwrap();
        let now = Instant::now();
        notify_until_changed(&mut watcher, now);
        assert!(watcher.poll(now).is_none());
        let config = watcher.poll(now + RELOAD_DEBOUNCE).unwrap().unwrap();
        assert!(!config.bindings.is_empty());
    }

    #[test]
    fn atomic_rename_and_unrelated_files_are_filtered() {
        let directory = temporary_directory();
        let path = directory.join("config.kdl");
        let other = directory.join("other");
        std::fs::write(&path, "").unwrap();
        let mut watcher = ConfigWatcher::new(path.clone()).unwrap();
        watcher.duplicate_fd().unwrap();
        std::fs::write(&other, "unrelated").unwrap();
        watcher.notify(Instant::now()).unwrap();
        assert!(watcher.reload_at.is_none());

        let replacement = directory.join("replacement");
        std::fs::write(&replacement, "").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let now = Instant::now();
        notify_until_changed(&mut watcher, now);
        assert!(watcher.poll(now + RELOAD_DEBOUNCE).unwrap().is_ok());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(other).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn first_parent_creation_rearms_watch_to_closer_directory() {
        let directory = temporary_directory();
        let path = directory.join("new").join("config.kdl");
        let mut watcher = ConfigWatcher::new(path.clone()).unwrap();
        watcher.duplicate_fd().unwrap();
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        let now = Instant::now();
        notify_until_changed(&mut watcher, now);
        assert_eq!(watcher.watched_directory, path.parent().unwrap());
        assert!(watcher.poll(now + RELOAD_DEBOUNCE).unwrap().is_ok());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(path.parent().unwrap()).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn same_path_directory_replacement_rearms_new_inode() {
        let directory = temporary_directory();
        let config_directory = directory.join("config");
        let old_directory = directory.join("old");
        std::fs::create_dir(&config_directory).unwrap();
        let path = config_directory.join("config.kdl");
        std::fs::write(&path, "").unwrap();
        let mut watcher = ConfigWatcher::new(path.clone()).unwrap();
        watcher.duplicate_fd().unwrap();

        std::fs::rename(&config_directory, &old_directory).unwrap();
        std::fs::create_dir(&config_directory).unwrap();
        std::fs::write(&path, "").unwrap();
        let now = Instant::now();
        notify_until_changed(&mut watcher, now);
        let _ = watcher.poll(now + RELOAD_DEBOUNCE).unwrap();

        std::fs::write(&path, "general { gap 9 }").unwrap();
        let later = now + RELOAD_DEBOUNCE + Duration::from_millis(1);
        notify_until_changed(&mut watcher, later);
        assert_eq!(
            watcher.poll(later + RELOAD_DEBOUNCE).unwrap().unwrap().gap,
            9
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(config_directory).unwrap();
        std::fs::remove_file(old_directory.join("config.kdl")).unwrap();
        std::fs::remove_dir(old_directory).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
