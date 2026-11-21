//! Reading a folder: one `readdir` plus one `stat` per entry, sorted folders
//! first with natural ordering ("file2" before "file10"), case-insensitive.
use std::cmp::Ordering;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// The name as shown: control characters and invalid UTF-8 as "?".
    pub label: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    pub mime: String,
}

impl Entry {
    pub fn hidden(&self) -> bool {
        self.name.starts_with('.') || self.name.ends_with('~')
    }
    pub fn executable(&self) -> bool {
        !self.is_dir && self.mode & 0o111 != 0
    }
}

pub fn read(path: &Path, mime: &crate::mime::Mime) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for item in std::fs::read_dir(path)? {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        let p = item.path();
        let lmeta = match item.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_link = lmeta.file_type().is_symlink();
        // A link shows what it points at; a broken link stays a link.
        let meta = if is_link {
            std::fs::metadata(&p).unwrap_or(lmeta)
        } else {
            lmeta
        };
        let is_dir = meta.is_dir();
        let mime_type = if is_dir {
            "inode/directory".to_string()
        } else {
            mime.for_name(&name).to_string()
        };
        let label: String = name
            .chars()
            .map(|c| {
                if c.is_control() || c == '\u{fffd}' {
                    '?'
                } else {
                    c
                }
            })
            .collect();
        out.push(Entry {
            label,
            name,
            path: p,
            is_dir,
            is_link,
            size: meta.len(),
            mtime: meta.mtime(),
            mode: meta.mode(),
            mime: mime_type,
        });
    }
    sort(&mut out, SortKey::Name, false);
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Type,
}

pub fn sort(v: &mut [Entry], key: SortKey, reverse: bool) {
    v.sort_by(|a, b| {
        let dirs = b.is_dir.cmp(&a.is_dir);
        if dirs != Ordering::Equal {
            return dirs;
        }
        let o = match key {
            SortKey::Name => Ordering::Equal,
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Modified => a.mtime.cmp(&b.mtime),
            SortKey::Type => a.mime.cmp(&b.mime),
        }
        .then_with(|| natural(&a.name, &b.name));
        if reverse {
            o.reverse()
        } else {
            o
        }
    });
}

/// Natural, case-insensitive comparison: digit runs compare as numbers.
pub fn natural(a: &str, b: &str) -> Ordering {
    let mut x = a.chars().peekable();
    let mut y = b.chars().peekable();
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n.push(c);
                    x.next();
                }
                let mut m = String::new();
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    m.push(d);
                    y.next();
                }
                let (n2, m2) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let o = n2.len().cmp(&m2.len()).then_with(|| n2.cmp(m2));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

/// Local date and time, "2026-11-21 18:04".
pub fn human_time(t: i64) -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let tt = t as libc::time_t;
    unsafe { libc::localtime_r(&tt, &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

/// Free and total bytes of the file system holding `path`.
pub fn space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0)
        .then(|| (s.f_bavail * s.f_frsize, s.f_blocks * s.f_frsize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_counts_numbers_and_ignores_case() {
        let mut v = vec!["file10", "File2", "file1", "a", "B"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, ["a", "B", "file1", "File2", "file10"]);
    }

    #[test]
    fn sizes_read_like_humans_write_them() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1500), "1.5 kB");
        assert_eq!(human_size(42_000_000), "42 MB");
    }
}
