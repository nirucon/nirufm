//! "Open with": the applications whose desktop entries list a file's type,
//! the user's choice from mimeapps.list first. Exec lines are parsed by the
//! desktop entry rules (quoting and field codes), and a program starts
//! directly, without a shell.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct App {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub terminal: bool,
}

pub struct Apps {
    by_mime: HashMap<String, Vec<App>>,
    defaults: HashMap<String, Vec<String>>,
}

fn localized<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut in_entry = false;
    for l in text.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_entry = l == "[Desktop Entry]";
            continue;
        }
        if in_entry {
            if let Some(v) = l.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
                return Some(v.trim());
            }
        }
    }
    None
}

impl Apps {
    pub fn load() -> Self {
        let mut by_mime: HashMap<String, Vec<App>> = HashMap::new();
        let mut seen = std::collections::HashSet::new();
        // User entries first: they hide system entries with the same id.
        for d in crate::mime::data_dirs() {
            let Ok(rd) = std::fs::read_dir(d.join("applications")) else {
                continue;
            };
            for e in rd.flatten() {
                let id = e.file_name().to_string_lossy().into_owned();
                if !id.ends_with(".desktop") || !seen.insert(id.clone()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(e.path()) else {
                    continue;
                };
                if localized(&text, "NoDisplay") == Some("true")
                    || localized(&text, "Hidden") == Some("true")
                {
                    continue;
                }
                let (Some(name), Some(exec), Some(types)) = (
                    localized(&text, "Name"),
                    localized(&text, "Exec"),
                    localized(&text, "MimeType"),
                ) else {
                    continue;
                };
                let app = App {
                    id: id.clone(),
                    name: name.to_string(),
                    exec: exec.to_string(),
                    terminal: localized(&text, "Terminal") == Some("true"),
                };
                for t in types.split(';').filter(|t| !t.is_empty()) {
                    by_mime.entry(t.to_string()).or_default().push(app.clone());
                }
            }
        }
        let mut defaults: HashMap<String, Vec<String>> = HashMap::new();
        let mut files = vec![crate::theme::config_home().join("mimeapps.list")];
        files.extend(
            crate::mime::data_dirs()
                .iter()
                .map(|d| d.join("applications/mimeapps.list")),
        );
        for f in files {
            let Ok(text) = std::fs::read_to_string(f) else {
                continue;
            };
            let mut section = String::new();
            for l in text.lines() {
                let l = l.trim();
                if l.starts_with('[') {
                    section = l.to_string();
                    continue;
                }
                if section != "[Default Applications]" && section != "[Added Associations]" {
                    continue;
                }
                if let Some((mime, ids)) = l.split_once('=') {
                    let list = defaults.entry(mime.to_string()).or_default();
                    for id in ids.split(';').filter(|s| !s.is_empty()) {
                        if !list.iter().any(|x| x == id) {
                            list.push(id.to_string());
                        }
                    }
                }
            }
        }
        Apps { by_mime, defaults }
    }

    /// Applications for a type: defaults first, then the rest by name.
    pub fn for_mime(&self, mime: &str) -> Vec<App> {
        let mut v: Vec<App> = Vec::new();
        let all: Vec<App> = self.by_mime.get(mime).cloned().unwrap_or_default();
        let generic = format!("{}/*", mime.split('/').next().unwrap_or(""));
        let wide: Vec<App> = self.by_mime.get(&generic).cloned().unwrap_or_default();
        if let Some(ids) = self.defaults.get(mime) {
            for id in ids {
                if let Some(a) = all.iter().chain(wide.iter()).find(|a| &a.id == id) {
                    v.push(a.clone());
                }
            }
        }
        let mut rest: Vec<App> = all
            .into_iter()
            .chain(wide)
            .filter(|a| !v.iter().any(|x| x.id == a.id))
            .collect();
        rest.sort_by_key(|a| a.name.to_lowercase());
        rest.dedup_by(|a, b| a.id == b.id);
        v.extend(rest);
        v
    }
}

/// Split an Exec value into words (desktop entry quoting rules).
pub fn split_exec(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' | '\t' if !quoted => {
                if any || !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            _ => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// The command line for opening `files` with an Exec value.
pub fn command(exec: &str, files: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    let mut used = false;
    for w in split_exec(exec) {
        match w.as_str() {
            "%f" | "%u" => {
                if let Some(f) = files.first() {
                    out.push(if w == "%u" {
                        crate::x::file_uri(f)
                    } else {
                        f.to_string_lossy().into_owned()
                    });
                }
                used = true;
            }
            "%F" | "%U" => {
                for f in files {
                    out.push(if w == "%U" {
                        crate::x::file_uri(f)
                    } else {
                        f.to_string_lossy().into_owned()
                    });
                }
                used = true;
            }
            "%i" | "%c" | "%k" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {}
            _ => out.push(w.replace("%%", "%")),
        }
    }
    if !used {
        out.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
    }
    out
}

pub fn launch(app: &App, files: &[PathBuf], cwd: &Path) {
    let mut cmd = command(&app.exec, files);
    if app.terminal {
        let mut t = vec!["kitty".to_string()];
        t.append(&mut cmd);
        cmd = t;
    }
    let Some((prog, args)) = cmd.split_first() else {
        return;
    };
    let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    crate::app::spawn(prog, &args, Some(cwd));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_lines_are_unquoted_and_field_codes_filled() {
        assert_eq!(
            split_exec(r#""/home/u/.local/bin/helium" %U"#),
            ["/home/u/.local/bin/helium", "%U"]
        );
        assert_eq!(
            split_exec(r#"sh -c "echo \"hi\"""#),
            ["sh", "-c", "echo \"hi\""]
        );
        let f = vec![PathBuf::from("/tmp/a b.png")];
        assert_eq!(command("gimp %F", &f), ["gimp", "/tmp/a b.png"]);
        assert_eq!(
            command("viewer %u --x", &f),
            ["viewer", "file:///tmp/a%20b.png", "--x"]
        );
        assert_eq!(command("player", &f), ["player", "/tmp/a b.png"]);
        assert_eq!(command("app %i %f", &f), ["app", "/tmp/a b.png"]);
    }
}
