//! The side panel: the XDG user folders, Nextcloud, the trash, the file
//! system and mounted drives.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub label: String,
    pub path: PathBuf,
    pub icon: &'static str,
    pub device: bool,
    /// A removable drive: its block device, for mounting and ejecting.
    pub dev: Option<String>,
    pub mounted: bool,
    /// A server: its URI, to connect (bookmark) or disconnect (mounted).
    pub uri: Option<String>,
    /// The "Connect to server" row.
    pub connect: bool,
    /// The heading this row is listed under ("" for the first group).
    pub section: &'static str,
    pub bookmark: bool,
}

impl Place {
    fn local(label: &str, path: PathBuf, icon: &'static str) -> Self {
        Place {
            label: label.into(),
            path,
            icon,
            device: false,
            dev: None,
            mounted: true,
            uri: None,
            connect: false,
            section: "",
            bookmark: false,
        }
    }
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
    let mut v = vec![Place::local("Home", home.clone(), "user-home")];
    let mut recent = Place::local("Recent", PathBuf::new(), "document-open-recent");
    recent.mounted = false;
    v.push(recent);
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
            v.push(Place::local(label, p, icon));
        }
    }
    for nc in crate::nextcloud::sync_folders() {
        let label = if nc == home.join("Nextcloud") {
            "Nextcloud".to_string()
        } else {
            nc.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Nextcloud".into())
        };
        v.push(Place::local(&label, nc, "folder-nextcloud"));
    }
    if !v.iter().any(|p| p.icon == "folder-nextcloud") && home.join("Nextcloud").is_dir() {
        v.push(Place::local(
            "Nextcloud",
            home.join("Nextcloud"),
            "folder-nextcloud",
        ));
    }
    v.push(Place::local("Trash", trash_files(), "user-trash"));
    v.push(Place::local(
        "File system",
        PathBuf::from("/"),
        "drive-harddisk",
    ));
    for b in crate::remote::bookmarks() {
        let local = b.uri.strip_prefix("file://").map(crate::ops::decode_path);
        let mut pl = Place::local(&b.label, local.clone().unwrap_or_default(), "folder");
        pl.section = "BOOKMARKS";
        pl.bookmark = true;
        if local.is_none() {
            pl.uri = Some(b.uri.clone());
            pl.icon = "folder-remote";
            pl.mounted = false;
            // A server bookmark that is connected opens its folder.
            if let Some(m) = crate::remote::mounted()
                .into_iter()
                .find(|m| b.uri.starts_with(&m.uri))
            {
                pl.path = m.path;
                pl.mounted = true;
            }
        }
        v.push(pl);
    }
    v.extend(drives());
    for m in crate::remote::mounted() {
        let mut pl = Place::local(&m.label, m.path, "network-server");
        pl.section = "NETWORK";
        pl.uri = Some(m.uri);
        v.push(pl);
    }
    let mut c = Place::local("Connect to server…", PathBuf::new(), "network-workgroup");
    c.section = "NETWORK";
    c.connect = true;
    c.mounted = false;
    v.push(c);
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
    let removable = crate::devices::removable();
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
        // A plugged-in drive is listed below with its eject button.
        if removable
            .iter()
            .any(|d| d.mount.as_deref() == Some(Path::new(&mnt)))
        {
            continue;
        }
        v.push(Place {
            label,
            path: PathBuf::from(mnt),
            icon,
            device: true,
            dev: None,
            mounted: true,
            uri: None,
            connect: false,
            section: "DEVICES",
            bookmark: false,
        });
    }
    for d in removable {
        let icon = if d.dev.contains("mmcblk") {
            "media-flash"
        } else {
            "drive-removable-media-usb"
        };
        v.push(Place {
            label: d.label.clone(),
            path: d.mount.clone().unwrap_or_default(),
            icon,
            device: true,
            mounted: d.mount.is_some(),
            uri: None,
            connect: false,
            section: "DEVICES",
            bookmark: false,
            dev: Some(d.dev),
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

/// Recently used files and folders (`recently-used.xbel`, which every
/// GTK program writes), newest first, existing only. Read when Recent is
/// opened, never before.
pub fn recent_files(limit: usize) -> Vec<PathBuf> {
    let file = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::theme::home().join(".local/share"))
        .join("recently-used.xbel");
    let text = std::fs::read_to_string(file).unwrap_or_default();
    parse_recent(&text)
        .into_iter()
        .filter(|p| p.exists())
        .take(limit)
        .collect()
}

/// The local paths in an xbel file, newest first, each once.
fn parse_recent(text: &str) -> Vec<PathBuf> {
    let mut items: Vec<(String, PathBuf)> = Vec::new();
    for chunk in text.split("<bookmark ").skip(1) {
        // Only the attributes of the bookmark element itself.
        let head = chunk.split('>').next().unwrap_or("");
        let attr = |name: &str| {
            head.split(&format!(" {name}=\""))
                .nth(1)
                .or_else(|| head.strip_prefix(&format!("{name}=\"")))
                .and_then(|r| r.split('"').next())
                .map(str::to_string)
        };
        let (Some(href), Some(when)) = (attr("href"), attr("modified").or_else(|| attr("visited")))
        else {
            continue;
        };
        if let Some(rest) = href.strip_prefix("file://") {
            items.push((when, crate::ops::decode_path(&xml_unescape(rest))));
        }
    }
    // The times are ISO 8601 in UTC, so they sort as text.
    items.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|(_, p)| seen.insert(p.clone()))
        .map(|(_, p)| p)
        .collect()
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points_with_spaces_are_unescaped() {
        assert_eq!(unescape("/media/niru/My\\040Stick"), "/media/niru/My Stick");
        assert_eq!(unescape("/mnt/plain"), "/mnt/plain");
    }

    #[test]
    fn recent_files_newest_first_once_each() {
        let xbel = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0">
  <bookmark href="file:///home/u/old.txt" added="2026-01-01T10:00:00Z" modified="2026-01-01T10:00:00Z" visited="2026-01-01T10:00:00Z">
    <info><metadata><mime:mime-type type="text/plain"/></metadata></info>
  </bookmark>
  <bookmark href="file:///home/u/Tom%20%26%20Jerry.png" added="x" modified="2026-03-01T10:00:00Z" visited="x"/>
  <bookmark href="file:///home/u/R&amp;D" modified="2026-02-01T10:00:00Z"/>
  <bookmark href="https://example.com/x" modified="2026-04-01T10:00:00Z"/>
  <bookmark href="file:///home/u/old.txt" modified="2025-01-01T10:00:00Z"/>
</xbel>"#;
        let v = parse_recent(xbel);
        assert_eq!(
            v,
            [
                PathBuf::from("/home/u/Tom & Jerry.png"),
                PathBuf::from("/home/u/R&D"),
                PathBuf::from("/home/u/old.txt"),
            ]
        );
    }
}
