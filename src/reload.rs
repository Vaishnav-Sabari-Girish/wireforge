use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

/// How often the file is stat-ed (mtime + length).
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The result of one poll of the watched file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadEvent {
    /// File state unchanged since the last poll (or the poll was throttled).
    Unchanged,
    /// The file changed on disk (mtime or length moved) and now exists:
    Changed,
    /// The file disappeared (deleted or renamed away): keep the last model.
    Missing,
}

/// File identity used for polling: modification time + length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

fn stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        modified: meta.modified().ok(),
        len: meta.len(),
    })
}

/// Polls a single file for external changes, throttled to one stat per POLL_INTERVAL.
pub struct ReloadWatch {
    path: PathBuf,
    last: Option<FileStamp>,
    next_poll: Instant,
    interval: Duration,
}

impl ReloadWatch {
    /// Watch `path` with the default [`POLL_INTERVAL`].
    pub fn new(path: PathBuf) -> Self {
        Self::with_interval(path, POLL_INTERVAL)
    }

    /// Watch `path`, stat-ing at most once per `interval`.
    pub fn with_interval(path: PathBuf, interval: Duration) -> Self {
        Self {
            path,
            last: None,
            next_poll: Instant::now() + interval,
            interval,
        }
    }

    /// Record the current file state as the baseline.
    pub fn baseline(&mut self) {
        self.last = stamp(&self.path);
    }

    /// Poll the file; returns the transition since the last poll.
    pub fn poll(&mut self) -> ReloadEvent {
        let now = Instant::now();
        if now < self.next_poll {
            return ReloadEvent::Unchanged;
        }
        self.next_poll = now + self.interval;

        let cur = stamp(&self.path);
        let event = match (cur, self.last) {
            // Gone -> gone: nothing new.
            (None, None) => ReloadEvent::Unchanged,
            // Present -> gone: the file was deleted or renamed away.
            (None, Some(_)) => ReloadEvent::Missing,
            // Gone -> present: the file was re-created; reload it.
            (Some(_), None) => ReloadEvent::Changed,
            // Present -> present: changed only when mtime or length moved.
            (Some(s), Some(prev)) => {
                if s == prev {
                    ReloadEvent::Unchanged
                } else {
                    ReloadEvent::Changed
                }
            }
        };
        self.last = cur;
        event
    }
}

// Event-driven inotify reload (Linux).

/// Spawn the inotify hot-reload thread for `target_file` (Linux).
#[cfg(target_os = "linux")]
pub fn spawn_inotify(
    target_file: &Path,
    tx: Sender<crate::LoopEvent>,
) -> Option<std::thread::JoinHandle<()>> {
    use inotify::{Inotify, WatchMask};

    let path = target_file.to_path_buf();
    let parent = path.parent()?.to_path_buf();
    let name = path.file_name()?.to_owned();

    std::thread::Builder::new()
        .name("wireforge-inotify".into())
        .spawn(move || {
            let Ok(mut inotify) = Inotify::init() else {
                return;
            };
            let Ok(_dir_wd) = inotify.watches().add(
                &parent,
                WatchMask::CLOSE_WRITE
                    | WatchMask::CREATE
                    | WatchMask::MOVED_TO
                    | WatchMask::MOVED_FROM
                    | WatchMask::DELETE,
            ) else {
                return;
            };
            let file_mask = WatchMask::MODIFY
                | WatchMask::CLOSE_WRITE
                | WatchMask::DELETE_SELF
                | WatchMask::MOVE_SELF;
            let mut file_wd = inotify.watches().add(&path, file_mask).ok();

            let mut buf = [0u8; 4096];
            loop {
                let Ok(events) = inotify.read_events_blocking(&mut buf) else {
                    return;
                };
                let mut file_gone = false;
                let mut file_changed = false;
                for event in events {
                    // The file watch's self-events carry no name; the parent
                    // watch's events carry the entry name (match it).
                    let name_match = event.name.as_ref().is_none_or(|n| *n == name);
                    if !name_match {
                        continue;
                    }
                    let mask = event.mask;
                    if mask.intersects(
                        inotify::EventMask::DELETE
                            | inotify::EventMask::MOVED_FROM
                            | inotify::EventMask::DELETE_SELF,
                    ) {
                        file_gone = true;
                    }
                    if mask.intersects(
                        inotify::EventMask::CLOSE_WRITE
                            | inotify::EventMask::CREATE
                            | inotify::EventMask::MOVED_TO
                            | inotify::EventMask::MODIFY
                            | inotify::EventMask::MOVE_SELF,
                    ) {
                        file_changed = true;
                    }
                }
                // Re-arm the file watch after it was invalidated by a move
                // or delete, so later in-place writes are still caught.
                if file_gone && let Some(wd) = file_wd.take() {
                    let _ = inotify.watches().remove(wd);
                    file_wd = inotify.watches().add(&path, file_mask).ok();
                }
                let event = if file_changed && path.exists() {
                    Some(ReloadEvent::Changed)
                } else if file_gone {
                    Some(ReloadEvent::Missing)
                } else {
                    None
                };
                if let Some(ev) = event
                    && tx.send(crate::LoopEvent::Reload(ev)).is_err()
                {
                    return;
                }
            }
        })
        .ok()
}

/// Non-Linux: no inotify thread (the caller uses the mtime-poll watch).
#[cfg(not(target_os = "linux"))]
pub fn spawn_inotify(
    _target_file: &Path,
    _tx: Sender<crate::LoopEvent>,
) -> Option<std::thread::JoinHandle<()>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::SystemTime;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A unique temp file path inside a per-process scratch dir.
    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wrfm-reload-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(format!(
            "{name}-{}.wrfm",
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// A watch that stats on every poll (no throttle).
    fn instant_watch(path: PathBuf) -> ReloadWatch {
        ReloadWatch::with_interval(path, Duration::ZERO)
    }

    #[test]
    fn baseline_then_unchanged() {
        let p = temp_path("static");
        fs::write(&p, "v 0 0 0\n").unwrap();
        let mut w = instant_watch(p);
        w.baseline();
        assert_eq!(w.poll(), ReloadEvent::Unchanged);
    }

    #[test]
    fn modified_content_reports_changed() {
        let p = temp_path("mod");
        fs::write(&p, "v 0 0 0\n").unwrap();
        let mut w = instant_watch(p.clone());
        w.baseline();
        fs::write(&p, "v 0 0 0\nv 1 1 1\n").unwrap();
        assert_eq!(w.poll(), ReloadEvent::Changed);
    }

    #[test]
    fn same_length_rewrite_reports_changed_when_mtime_moves() {
        // Same byte length, different content: length alone cannot detect it,
        // so the poll must rely on the mtime moving. Set a distinct mtime to
        // make the test deterministic.
        let p = temp_path("same-len");
        fs::write(&p, "v 0 0 0\n").unwrap();
        let mut w = instant_watch(p.clone());
        w.baseline();
        fs::write(&p, "v 9 9 9\n").unwrap();
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(SystemTime::now()).unwrap();
        assert_eq!(w.poll(), ReloadEvent::Changed);
    }

    #[test]
    fn deleted_file_reports_missing_then_stays_quiet() {
        let p = temp_path("del");
        fs::write(&p, "v 0 0 0\n").unwrap();
        let mut w = instant_watch(p.clone());
        w.baseline();
        fs::remove_file(&p).unwrap();
        assert_eq!(w.poll(), ReloadEvent::Missing);
        assert_eq!(w.poll(), ReloadEvent::Unchanged);
    }

    #[test]
    fn recreated_file_reports_changed_after_missing() {
        let p = temp_path("recreate");
        fs::write(&p, "v 0 0 0\n").unwrap();
        let mut w = instant_watch(p.clone());
        w.baseline();
        fs::remove_file(&p).unwrap();
        assert_eq!(w.poll(), ReloadEvent::Missing);
        fs::write(&p, "v 0 0 0\nv 1 1 1\n").unwrap();
        assert_eq!(w.poll(), ReloadEvent::Changed);
    }

    #[test]
    fn missing_file_at_start_stays_quiet_until_it_appears() {
        let p = temp_path("later");
        let mut w = instant_watch(p.clone());
        w.baseline();
        assert_eq!(w.poll(), ReloadEvent::Unchanged);
        fs::write(&p, "v 0 0 0\n").unwrap();
        assert_eq!(w.poll(), ReloadEvent::Changed);
    }
}
