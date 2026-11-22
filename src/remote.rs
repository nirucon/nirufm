//! Servers (sftp, ftp, smb, dav) through gvfs: `gio mount URI`, after which
//! the files are ordinary files under `/run/user/UID/gvfs` (gvfsd-fuse), so
//! every operation of the file manager works on them. SSH keys work as in a
//! terminal; a password is asked for only when the server wants one.
//!
//! Bookmarks are the GTK list (`~/.config/gtk-3.0/bookmarks`), shared with
//! the GTK file chooser and other file managers.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn gvfs_root() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")))
        .join("gvfs")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mounted {
    pub label: String,
    pub path: PathBuf,
    pub uri: String,
}

/// "sftp:host=othala,user=niru" -> ("sftp", [("host","othala"),("user","niru")])
fn split_name(name: &str) -> Option<(String, Vec<(String, String)>)> {
    let (scheme, rest) = name.split_once(':')?;
    let kv = rest
        .split(',')
        .filter_map(|p| p.split_once('=').map(|(k, v)| (k.to_string(), unescape(v))))
        .collect();
    Some((scheme.to_string(), kv))
}

/// gvfs escapes "%" and "," in values as %XX.
fn unescape(s: &str) -> String {
    crate::ops::decode_path(s).to_string_lossy().into_owned()
}

pub fn uri_of(name: &str) -> Option<(String, String)> {
    let (scheme, kv) = split_name(name)?;
    let get = |k: &str| kv.iter().find(|(a, _)| a == k).map(|(_, b)| b.clone());
    let host = get("host").or_else(|| get("server"))?;
    let user = get("user").map(|u| format!("{u}@")).unwrap_or_default();
    let port = get("port").map(|p| format!(":{p}")).unwrap_or_default();
    let (scheme, path) = match scheme.as_str() {
        "smb-share" => (
            "smb".to_string(),
            format!("/{}", get("share").unwrap_or_default()),
        ),
        "dav" | "davs" => (scheme.clone(), get("prefix").unwrap_or_default()),
        _ => (scheme.clone(), String::new()),
    };
    let label = format!(
        "{user}{host}{}",
        if path.is_empty() {
            String::new()
        } else {
            path.clone()
        }
    );
    Some((
        format!("{scheme}://{user}{host}{port}{path}"),
        format!("{label} ({scheme})"),
    ))
}

pub fn mounted() -> Vec<Mounted> {
    let Ok(rd) = std::fs::read_dir(gvfs_root()) else {
        return Vec::new();
    };
    let mut v: Vec<Mounted> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let (uri, label) = uri_of(&name)?;
            Some(Mounted {
                label,
                path: e.path(),
                uri,
            })
        })
        .collect();
    v.sort_by(|a, b| a.label.cmp(&b.label));
    v
}

#[derive(Debug)]
pub enum ConnectError {
    /// The server wants a user name and password.
    NeedsLogin,
    Failed(String),
}

/// Mount `uri`; `login` answers gio's prompts. Returns the local folder.
pub fn connect(uri: &str, login: Option<(&str, &str)>) -> Result<PathBuf, ConnectError> {
    if !crate::archive::have("gio") {
        return Err(ConnectError::Failed(
            "gio is missing (install gvfs-backends and gvfs-fuse)".into(),
        ));
    }
    let before: Vec<PathBuf> = mounted().into_iter().map(|m| m.path).collect();
    let mut child = Command::new("gio")
        .args(["mount", uri])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ConnectError::Failed(e.to_string()))?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take();
        if let (Some(si), Some((user, pass))) = (stdin.as_mut(), login) {
            // gio asks "User", for smb also "Domain", then "Password".
            let domain = if uri.starts_with("smb://") {
                "WORKGROUP\n"
            } else {
                ""
            };
            let _ = write!(si, "{user}\n{domain}{pass}\n");
        }
        drop(stdin);
    }
    let out = child
        .wait_with_output()
        .map_err(|e| ConnectError::Failed(e.to_string()))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let already = text.contains("already mounted");
    if !out.status.success() && !already {
        let lower = text.to_lowercase();
        let msg = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed")
            .trim()
            .to_string();
        if login.is_none()
            && (lower.contains("password")
                || lower.contains("permission denied")
                || lower.contains("login"))
        {
            return Err(ConnectError::NeedsLogin);
        }
        return Err(ConnectError::Failed(msg));
    }
    let now = mounted();
    if let Some(m) = now.iter().find(|m| !before.contains(&m.path)) {
        return Ok(m.path.clone());
    }
    // Mounted before: find it by its host.
    let host = uri
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    now.into_iter()
        .find(|m| m.uri.contains(host))
        .map(|m| m.path)
        .ok_or_else(|| {
            ConnectError::Failed(
                "mounted, but its folder was not found (is gvfsd-fuse running?)".into(),
            )
        })
}

pub fn disconnect(uri: &str) -> Result<(), String> {
    let out = Command::new("gio")
        .args(["mount", "-u", uri])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Host aliases from ~/.ssh/config, offered as sftp:// suggestions.
pub fn ssh_hosts() -> Vec<String> {
    let text =
        std::fs::read_to_string(crate::theme::home().join(".ssh/config")).unwrap_or_default();
    let mut v: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l
                .strip_prefix("Host ")
                .or_else(|| l.strip_prefix("Host\t"))?;
            Some(
                rest.split_whitespace()
                    .filter(|h| !h.contains(['*', '?', '!']))
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();
    v.dedup();
    v
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub uri: String,
    pub label: String,
}

fn bookmarks_file() -> PathBuf {
    crate::theme::config_home().join("gtk-3.0/bookmarks")
}

pub fn bookmarks() -> Vec<Bookmark> {
    let text = std::fs::read_to_string(bookmarks_file()).unwrap_or_default();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (uri, label) = l
                .split_once(' ')
                .map(|(u, n)| (u.to_string(), n.to_string()))
                .unwrap_or((l.to_string(), String::new()));
            let label = if label.is_empty() {
                let path = uri.trim_end_matches('/');
                let last = path.rsplit('/').next().unwrap_or(path);
                crate::ops::decode_path(last).to_string_lossy().into_owned()
            } else {
                label
            };
            Bookmark { uri, label }
        })
        .collect()
}

/// Add a folder to the bookmarks (once).
pub fn add_bookmark(path: &Path) -> std::io::Result<bool> {
    let uri = crate::x::file_uri(path);
    let file = bookmarks_file();
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if text
        .lines()
        .any(|l| l.split(' ').next() == Some(uri.as_str()))
    {
        return Ok(false);
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut body = text;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&uri);
    body.push('\n');
    std::fs::write(&file, body)?;
    Ok(true)
}

pub fn remove_bookmark(uri: &str) -> std::io::Result<()> {
    let file = bookmarks_file();
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| l.split(' ').next() != Some(uri))
        .collect();
    std::fs::write(
        &file,
        if kept.is_empty() {
            String::new()
        } else {
            kept.join("\n") + "\n"
        },
    )
}

fn write_bookmark_lines(lines: &[String]) -> std::io::Result<()> {
    let file = bookmarks_file();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    };
    // Written beside and renamed, so GTK programs never read half a list.
    let tmp = file.with_extension("nirufm-tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &file)
}

fn bookmark_lines() -> Vec<String> {
    std::fs::read_to_string(bookmarks_file())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Move the bookmark at position `from` to position `to` (in list order).
pub fn move_bookmark(from: usize, to: usize) -> std::io::Result<()> {
    let mut lines = bookmark_lines();
    if from >= lines.len() || from == to {
        return Ok(());
    }
    let to = to.min(lines.len() - 1);
    let l = lines.remove(from);
    lines.insert(to, l);
    write_bookmark_lines(&lines)
}

/// Give a bookmark its own name; an empty name goes back to the folder name.
pub fn rename_bookmark(uri: &str, label: &str) -> std::io::Result<()> {
    let label = label.trim().replace(['\n', '\r'], " ");
    let lines: Vec<String> = bookmark_lines()
        .into_iter()
        .map(|l| {
            if l.split(' ').next() == Some(uri) {
                if label.is_empty() {
                    uri.to_string()
                } else {
                    format!("{uri} {label}")
                }
            } else {
                l
            }
        })
        .collect();
    write_bookmark_lines(&lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_move_and_get_names() {
        let dir = std::env::temp_dir().join(format!("nirufm-bm-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("gtk-3.0")).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::fs::write(
            dir.join("gtk-3.0/bookmarks"),
            "file:///a\nfile:///b Bee\nsftp://h/c\n",
        )
        .unwrap();
        move_bookmark(2, 0).unwrap();
        rename_bookmark("file:///a", "Alpha one").unwrap();
        rename_bookmark("file:///b", "").unwrap();
        let text = std::fs::read_to_string(dir.join("gtk-3.0/bookmarks")).unwrap();
        assert_eq!(text, "sftp://h/c\nfile:///a Alpha one\nfile:///b\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gvfs_folder_names_become_uris() {
        assert_eq!(
            uri_of("sftp:host=othala,user=niru").unwrap(),
            (
                "sftp://niru@othala".to_string(),
                "niru@othala (sftp)".to_string()
            )
        );
        assert_eq!(
            uri_of("ftp:host=ftp.example.com,port=2121").unwrap().0,
            "ftp://ftp.example.com:2121"
        );
        assert_eq!(
            uri_of("smb-share:server=nas,share=music,user=niru")
                .unwrap()
                .0,
            "smb://niru@nas/music"
        );
        assert!(uri_of("nonsense").is_none());
    }
}
