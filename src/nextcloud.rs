//! Nextcloud links through the desktop client's own socket, the interface
//! the Nautilus and Dolphin plugins use. The client knows the account and
//! does the sharing; NIRUFM never sees a password. The client puts the link
//! on the clipboard itself and shows its notification.
use std::io::Write;
use std::path::{Path, PathBuf};

fn config() -> String {
    std::fs::read_to_string(crate::theme::config_home().join("Nextcloud/nextcloud.cfg"))
        .unwrap_or_default()
}

/// The local folders the client syncs (`N\Folders\M\localPath=`). A folder
/// inside another one is left out: the outer one already covers it.
pub fn sync_folders() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = config()
        .lines()
        .filter(|l| l.contains("\\Folders\\") && l.contains("\\localPath="))
        .filter_map(|l| {
            l.split_once('=')
                .map(|(_, p)| PathBuf::from(p.trim().trim_end_matches('/')))
        })
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v.dedup();
    let all = v.clone();
    v.retain(|p| !all.iter().any(|o| o != p && p.starts_with(o)));
    v
}

pub fn in_sync_folder(p: &Path) -> bool {
    sync_folders().iter().any(|f| p.starts_with(f))
}

fn socket() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")))
        .join("Nextcloud/socket")
}

pub fn running() -> bool {
    std::os::unix::net::UnixStream::connect(socket()).is_ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    PublicLink,
    InternalLink,
    Share,
    OpenInBrowser,
}

pub fn send(cmd: Command, path: &Path) -> Result<(), String> {
    let mut s = std::os::unix::net::UnixStream::connect(socket())
        .map_err(|_| "the Nextcloud client is not running".to_string())?;
    let verb = match cmd {
        Command::PublicLink => "COPY_PUBLIC_LINK",
        Command::InternalLink => "COPY_PRIVATE_LINK",
        Command::Share => "SHARE",
        Command::OpenInBrowser => "OPEN_PRIVATE_LINK",
    };
    // The client matches paths without a trailing slash.
    let p = path.to_string_lossy();
    let p = p.trim_end_matches('/');
    s.write_all(format!("{verb}:{p}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let _ = s.flush();
    // Give the client a moment to read before the socket closes.
    std::thread::sleep(std::time::Duration::from_millis(150));
    Ok(())
}

/// Sync state of each path from the client: 1 synced, 2 syncing, 3 error,
/// 0 unknown or ignored. Asked over one connection, answers read with a
/// short deadline.
pub fn statuses(paths: &[PathBuf]) -> Vec<(PathBuf, u8)> {
    use std::io::{BufRead, BufReader};
    let Ok(s) = std::os::unix::net::UnixStream::connect(socket()) else {
        return Vec::new();
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(400)));
    let mut w = &s;
    for p in paths {
        let q = p.to_string_lossy();
        let _ =
            w.write_all(format!("RETRIEVE_FILE_STATUS:{}\n", q.trim_end_matches('/')).as_bytes());
    }
    let _ = w.flush();
    let mut out = Vec::new();
    let reader = BufReader::new(&s);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        // STATUS:OK:/path, STATUS:SYNC:/path, STATUS:ERROR:/path, STATUS:OK+SWM:/path
        let Some(rest) = line.strip_prefix("STATUS:") else {
            continue;
        };
        let Some((st, path)) = rest.split_once(':') else {
            continue;
        };
        let code = match st.split('+').next().unwrap_or("") {
            "OK" => 1,
            "SYNC" | "NEW" => 2,
            "ERROR" | "WARNING" => 3,
            _ => 0,
        };
        out.push((PathBuf::from(path), code));
        if out.len() >= paths.len() {
            break;
        }
    }
    out
}
