use std::path::PathBuf;
use std::time::Duration;

fn lock_path() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        });
    base.join("opencode")
        .join("storage")
        .join("oh-my-opencode-slim")
        .join("companion.pid")
}

/// Reads the owner pid from the lock file, retrying briefly while it is empty.
///
/// `create_new` + writing the pid is not atomic: a racing process can observe
/// the file after it is created but before the owner has written its pid. Two
/// companions spawned together (e.g. OpenCode's server and TUI processes each
/// load the plugin) hit this window. Treating an empty file as immediately
/// stale lets the racer delete a live owner's lock, so BOTH end up believing
/// they hold it — two overlay windows fighting over the same viewport, which
/// looks like flicker. Give a just-created lock a short grace period to finish
/// writing before deciding it is stale.
fn read_owner_pid(path: &std::path::Path) -> Option<u32> {
    for attempt in 0..10 {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                if let Ok(pid) = content.trim().parse::<u32>() {
                    return Some(pid);
                }
                // File exists but pid not written yet: wait and re-read.
                std::thread::sleep(Duration::from_millis(20));
            }
            // Owner released the lock between our create_new and this read.
            Err(_) if attempt > 0 => return None,
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    None
}

/// Returns true if this process should continue running.
/// Returns false if another companion instance is already alive.
///
/// A single companion window aggregates every OpenCode session, so the lock is
/// global rather than per-session: whichever OpenCode process spawns the
/// companion first wins, and later spawns from other processes self-exit.
pub fn acquire() -> bool {
    let path = lock_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = write!(file, "{}", std::process::id());
                let _ = file.flush();
                crate::log::debug(format!(
                    "lock acquired pid={} path={}",
                    std::process::id(),
                    path.display()
                ));
                return true;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing_pid = read_owner_pid(&path);
                if existing_pid.is_some_and(|pid| pid != std::process::id() && is_alive(pid)) {
                    crate::log::debug(format!(
                        "lock duplicate existing_pid={:?} current_pid={}",
                        existing_pid,
                        std::process::id()
                    ));
                    return false;
                }
                crate::log::debug(format!(
                    "lock stale existing_pid={:?} current_pid={} path={}",
                    existing_pid,
                    std::process::id(),
                    path.display()
                ));
                let _ = std::fs::remove_file(&path);
            }
            Err(err) => {
                crate::log::debug(format!(
                    "lock error err={} path={}",
                    err,
                    path.display()
                ));
                return false;
            }
        }
    }

    false
}

/// Releases the global singleton lock if this process still owns it.
pub fn release() {
    let path = lock_path();
    let owned = std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| content.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid == std::process::id());
    if owned {
        let _ = std::fs::remove_file(&path);
        crate::log::debug(format!("lock released pid={}", std::process::id()));
    }
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    // kill -0 checks if the process exists without sending a signal
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn is_alive(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::read_owner_pid;
    use std::io::Write;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("companion-singleton-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn reads_written_pid() {
        let path = temp_path("written");
        let mut file = std::fs::File::create(&path).unwrap();
        write!(file, "4242").unwrap();
        drop(file);
        assert_eq!(read_owner_pid(&path), Some(4242));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_file_reads_as_no_owner_after_grace() {
        // An owner that never finishes writing (crashed mid-create) is
        // eventually treated as absent so the lock can be reclaimed.
        let path = temp_path("empty");
        std::fs::File::create(&path).unwrap();
        assert_eq!(read_owner_pid(&path), None);
        let _ = std::fs::remove_file(&path);
    }
}

