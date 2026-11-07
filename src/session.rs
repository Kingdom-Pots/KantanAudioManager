//! Crash recovery. Each running app holds a lock file. A lock left behind that
//! nothing holds marks a session that died, whose autosave and takes can be restored.

use std::fs::File;
use std::path::{Path, PathBuf};

const WAV_HEADER_SIZE: u64 = 100;

/// Where autosaves and in-progress takes live, outside any synced folder.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KANTAN_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    if cfg!(test) {
        return Some(std::env::temp_dir().join("kantan-unit-tests"));
    }
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    Some(base.join("KantanAudioManager"))
}

pub fn recordings_dir(root: &Path) -> PathBuf {
    root.join("recordings")
}

/// Opens a file so that nobody else can open it while it's held.
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(path)
}

/// What crashed sessions left behind.
#[derive(Debug, Default, PartialEq)]
pub struct Orphans {
    /// The newest autosave, if any.
    pub autosave: Option<PathBuf>,
    pub takes: Vec<PathBuf>,
    /// Everything to remove once these have been dealt with.
    leftovers: Vec<PathBuf>,
}

impl Orphans {
    pub fn is_empty(&self) -> bool {
        self.autosave.is_none() && self.takes.is_empty()
    }

    /// Deletes the leftovers, which are no longer needed.
    pub fn discard(&self) {
        for path in self.leftovers.iter().chain(&self.takes) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Clears the markers while leaving the takes for import to read.
    pub fn discard_markers(&self) {
        for path in &self.leftovers {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Finds sessions that ended without cleaning up. Running apps hold their lock,
/// so they're left alone.
pub fn find_orphans(root: &Path) -> Orphans {
    let mut orphans = Orphans::default();
    let Ok(entries) = std::fs::read_dir(root) else {
        return orphans;
    };
    let mut newest = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("session-").and_then(|n| n.strip_suffix(".lock")) else {
            continue;
        };
        if open_exclusive(&entry.path()).is_err() {
            continue;
        }
        orphans.leftovers.push(entry.path());

        let autosave = root.join(format!("autosave-{pid}.kantan"));
        if let Ok(modified) = std::fs::metadata(&autosave).and_then(|m| m.modified()) {
            orphans.leftovers.push(autosave.clone());
            if newest.as_ref().is_none_or(|(time, _)| modified > *time) {
                newest = Some((modified, autosave));
            }
        }

        let prefix = format!("take-{pid}-");
        if let Ok(takes) = std::fs::read_dir(recordings_dir(root)) {
            for path in takes.flatten().map(|t| t.path()) {
                if !path.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix)) {
                    continue;
                }
                // A file holding only a header, or less, has no audio to offer.
                let has_audio = std::fs::metadata(&path).is_ok_and(|m| m.len() > WAV_HEADER_SIZE);
                if has_audio {
                    orphans.takes.push(path);
                } else {
                    orphans.leftovers.push(path);
                }
            }
        }
    }
    orphans.autosave = newest.map(|(_, path)| path);
    orphans.takes.sort();
    orphans
}

/// This running app's claim on its files. Dropping it means a clean exit.
pub struct Session {
    lock: Option<File>,
    lock_path: PathBuf,
    autosave_path: PathBuf,
}

impl Session {
    pub fn start(root: &Path) -> Option<Self> {
        std::fs::create_dir_all(root).ok()?;
        let pid = std::process::id();
        let lock_path = root.join(format!("session-{pid}.lock"));
        let lock = open_exclusive(&lock_path).ok()?;
        Some(Self {
            lock: Some(lock),
            lock_path,
            autosave_path: root.join(format!("autosave-{pid}.kantan")),
        })
    }

    pub fn autosave_path(&self) -> &Path {
        &self.autosave_path
    }

    pub fn remove_autosave(&self) {
        let _ = std::fs::remove_file(&self.autosave_path);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.lock.take();
        let _ = std::fs::remove_file(&self.lock_path);
        self.remove_autosave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kantan_session_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(recordings_dir(&dir)).unwrap();
        dir
    }

    #[test]
    fn a_dead_session_is_found_and_can_be_discarded() {
        let root = folder("dead");
        std::fs::write(root.join("session-111.lock"), b"").unwrap();
        std::fs::write(root.join("autosave-111.kantan"), b"x").unwrap();
        let take = recordings_dir(&root).join("take-111-0.wav");
        std::fs::write(&take, [0; 200]).unwrap();
        let empty = recordings_dir(&root).join("take-111-1.wav");
        std::fs::write(&empty, b"").unwrap();
        std::fs::write(recordings_dir(&root).join("take-222-0.wav"), b"other").unwrap();

        let orphans = find_orphans(&root);
        assert_eq!(orphans.autosave, Some(root.join("autosave-111.kantan")));
        assert_eq!(orphans.takes.len(), 1);
        assert_eq!(orphans.takes[0], take);

        orphans.discard();
        assert!(find_orphans(&root).is_empty());
        assert!(!take.exists() && !empty.exists());
        assert!(recordings_dir(&root).join("take-222-0.wav").exists(), "not that session's");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_running_session_is_left_alone() {
        let root = folder("alive");
        let session = Session::start(&root).unwrap();
        std::fs::write(session.autosave_path(), b"x").unwrap();
        assert!(find_orphans(&root).is_empty());
        std::fs::remove_dir_all(recordings_dir(&root)).unwrap();
        drop(session);
        assert!(std::fs::read_dir(&root).unwrap().next().is_none(), "clean exit tidies up");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
