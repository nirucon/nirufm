//! The side panel: the XDG user folders, Nextcloud, the trash, the file
//! system and mounted drives.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub label: String,
    pub path: PathBuf,
    pub icon: &'static str,
    pub device: bool,
}

/// `~/.config/user-dirs.dirs`: XDG_DOWNLOAD_DIR="$HOME/Downloads".
pub fn user_dirs(home: &Path) -> Vec<(String, PathBuf)> {
    let path = crate::theme::config_home().join("user-dirs.dirs");
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.trim().split_once('=')?;
            let k = k.strip_prefix("XDG_")?.strip_suffix("_DIR")?;
            let v = v.trim().trim_matches('"');
            let p = v.strip_prefix("$HOME").map_or_else(
                || PathBuf::from(v),
                |rest| home.join(rest.trim_start_matches('/')),
            );
            Some((k.to_string(), p))
        })
        .collect()
}

pub fn places() -> Vec<Place> {
    let home = crate::theme::home();
    let dirs = user_dirs(&home);
    let dir = |key: &str, fallback: &str| {
        dirs.iter()
            .find(|(k, _)| k == key)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| home.join(fallback))
    };
    let mut v = vec![Place {
        label: "Home".into(),
        path: home.clone(),
        icon: "user-home",
        device: false,
    }];
    for (key, fallback, label, icon) in [
        ("DESKTOP", "Desktop", "Desktop", "user-desktop"),
        ("DOCUMENTS", "Documents", "Documents", "folder-documents"),
        ("DOWNLOAD", "Downloads", "Downloads", "folder-download"),
        ("MUSIC", "Music", "Music", "folder-music"),
        ("PICTURES", "Pictures", "Pictures", "folder-pictures"),
        ("VIDEOS", "Videos", "Videos", "folder-videos"),
    ] {
        let p = dir(key, fallback);
        if p.is_dir() && p != home {
            v.push(Place {
                label: label.into(),
                path: p,
                icon,
                device: false,
            });
        }
    }
    let nc = home.join("Nextcloud");
    if nc.is_dir() {
        v.push(Place {
            label: "Nextcloud".into(),
            path: nc,
            icon: "folder-nextcloud",
            device: false,
        });
    }
    let trash = trash_files();
    v.push(Place {
        label: "Trash".into(),
        path: trash,
        icon: "user-trash",
        device: false,
    });
    v.push(Place {
        label: "File system".into(),
        path: PathBuf::from("/"),
        icon: "drive-harddisk",
        device: false,
    });
    v.extend(drives());
    v
}

pub fn trash_files() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::theme::home().join(".local/share"))
        .join("Trash/files")
}

/// Removable and extra drives: mounts under /media, /run/media and /mnt.
pub fn drives() -> Vec<Place> {
    let text = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let mut v = Vec::new();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        let (Some(dev), Some(mnt)) = (f.next(), f.next()) else {
            continue;
        };
        let mnt = unescape(mnt);
        if !(mnt.starts_with("/media/")
            || mnt.starts_with("/run/media/")
            || mnt.starts_with("/mnt/"))
        {
            continue;
        }
        let label = Path::new(&mnt)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(mnt.clone());
        let icon = if dev.contains("mmcblk") {
            "media-flash"
        } else {
            "drive-removable-media"
        };
        v.push(Place {
            label,
            path: PathBuf::from(mnt),
            icon,
            device: true,
        });
    }
    v
}

/// /proc/mounts escapes space, tab, newline and backslash as \ooo.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() + 1 && b[i + 1..].len() >= 3 {
            if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(n);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// "~/Documents" for paths under the home folder.
pub fn pretty(p: &Path) -> String {
    let home = crate::theme::home();
    if p == home {
        return "~".into();
    }
    match p.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points_with_spaces_are_unescaped() {
        assert_eq!(unescape("/media/niru/My\\040Stick"), "/media/niru/My Stick");
        assert_eq!(unescape("/mnt/plain"), "/mnt/plain");
    }
}
