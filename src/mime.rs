//! File types from the shared MIME database (`/usr/share/mime/globs2` and
//! `generic-icons`), read once at start. Detection is by name, which is what
//! makes a large folder list instantly; content sniffing is left to the
//! programs that open the files.
use std::collections::HashMap;
use std::path::Path;

#[derive(Default)]
pub struct Mime {
    /// Lower-case extension (with any inner dots, e.g. "tar.gz") to type.
    ext: HashMap<String, (u32, String)>,
    /// Whole file names ("Makefile", "README").
    names: HashMap<String, String>,
    generic: HashMap<String, String>,
}

impl Mime {
    pub fn load() -> Self {
        let mut m = Mime::default();
        let dirs = data_dirs();
        for d in dirs.iter().rev() {
            m.read_globs(&d.join("mime/globs2"));
            m.read_generic(&d.join("mime/generic-icons"));
        }
        m
    }

    fn read_globs(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(4, ':');
            let (Some(w), Some(mime), Some(glob)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let weight: u32 = w.parse().unwrap_or(50);
            let case_sensitive = parts.next().is_some_and(|f| f.contains("cs"));
            if let Some(ext) = glob.strip_prefix("*.") {
                if ext.contains(['*', '?', '[']) {
                    continue;
                }
                let key = if case_sensitive {
                    ext.to_string()
                } else {
                    ext.to_lowercase()
                };
                let better = self.ext.get(&key).is_none_or(|(old, _)| weight > *old);
                if better {
                    self.ext.insert(key, (weight, mime.to_string()));
                }
            } else if !glob.contains(['*', '?', '[']) {
                self.names
                    .entry(glob.to_string())
                    .or_insert_with(|| mime.to_string());
            }
        }
    }

    fn read_generic(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        for line in text.lines() {
            if let Some((mime, icon)) = line.split_once(':') {
                self.generic.insert(mime.to_string(), icon.to_string());
            }
        }
    }

    /// The type of a file name. The longest matching extension wins, so
    /// "x.tar.gz" is a compressed tar archive, not just gzip.
    pub fn for_name(&self, name: &str) -> &str {
        if let Some(m) = self.names.get(name) {
            return m;
        }
        let lower = name.to_lowercase();
        let mut idx = 0;
        while let Some(dot) = lower[idx..].find('.') {
            let ext = &lower[idx + dot + 1..];
            let exact = &name[idx + dot + 1..];
            if let Some((_, m)) = self.ext.get(exact).or_else(|| self.ext.get(ext)) {
                return m;
            }
            idx += dot + 1;
        }
        "application/octet-stream"
    }

    /// Icon names to try for a type, most specific first.
    pub fn icon_names(&self, mime: &str) -> Vec<String> {
        let mut v = vec![mime.replace('/', "-")];
        if let Some(g) = self.generic.get(mime) {
            v.push(g.clone());
        }
        let top = mime.split('/').next().unwrap_or("");
        v.push(format!("{top}-x-generic"));
        v.push("text-x-generic".into());
        v.push("unknown".into());
        v
    }
}

pub fn data_dirs() -> Vec<std::path::PathBuf> {
    let mut v = vec![std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::theme::home().join(".local/share"))];
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let sys = if sys.is_empty() {
        "/usr/local/share:/usr/share".to_string()
    } else {
        sys
    };
    v.extend(
        sys.split(':')
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from),
    );
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Mime {
        let dir = std::env::temp_dir().join(format!("nirufm-mime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("globs2"),
            "# test\n50:application/gzip:*.gz\n55:application/x-compressed-tar:*.tar.gz\n\
             50:image/png:*.png\n50:text/x-makefile:Makefile\n50:text/x-c++src:*.C:cs\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("generic-icons"),
            "application/x-compressed-tar:package-x-generic\n",
        )
        .unwrap();
        let mut m = Mime::default();
        m.read_globs(&dir.join("globs2"));
        m.read_generic(&dir.join("generic-icons"));
        std::fs::remove_dir_all(dir).unwrap();
        m
    }

    #[test]
    fn the_longest_extension_and_whole_names_win() {
        let m = sample();
        assert_eq!(m.for_name("backup.tar.gz"), "application/x-compressed-tar");
        assert_eq!(m.for_name("x.gz"), "application/gzip");
        assert_eq!(m.for_name("PHOTO.PNG"), "image/png");
        assert_eq!(m.for_name("Makefile"), "text/x-makefile");
        assert_eq!(m.for_name("noext"), "application/octet-stream");
        assert_eq!(
            m.icon_names("application/x-compressed-tar")[1],
            "package-x-generic"
        );
    }
}
