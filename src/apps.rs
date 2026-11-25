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
    /// The program set to open each type ([Default Applications]; the
    /// user's file comes first and wins).
    chosen: HashMap<String, String>,
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
        let mut chosen: HashMap<String, String> = HashMap::new();
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
                    if section == "[Default Applications]" {
                        if let Some(id) = ids.split(';').find(|s| !s.is_empty()) {
                            chosen
                                .entry(mime.to_string())
                                .or_insert_with(|| id.to_string());
                        }
                    }
                    let list = defaults.entry(mime.to_string()).or_default();
                    for id in ids.split(';').filter(|s| !s.is_empty()) {
                        if !list.iter().any(|x| x == id) {
                            list.push(id.to_string());
                        }
                    }
                }
            }
        }
        Apps {
            by_mime,
            defaults,
            chosen,
        }
    }

    /// The id of the program that opens `mime` by default, if one is set.
    pub fn default_for(&self, mime: &str) -> Option<&str> {
        self.chosen.get(mime).map(String::as_str)
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

/// Make `id` the program that opens `mime`: its line in the
/// [Default Applications] part of `file` (mimeapps.list) is set, the rest
/// of the file stays as it was. The new file is written beside the old one
/// and renamed over it, so a crash never leaves half a file; a symbolic
/// link (a dotfiles setup) keeps pointing at the file it points at.
pub fn set_default(file: &Path, mime: &str, id: &str) -> std::io::Result<()> {
    use std::io::Write;
    let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let old = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let new = with_default(&old, mime, id);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_file_name(format!(".mimeapps.list.nirufm-{}", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(new.as_bytes())?;
        f.sync_all()?;
        if let Ok(m) = std::fs::metadata(&file) {
            std::fs::set_permissions(&tmp, m.permissions())?;
        }
        std::fs::rename(&tmp, &file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `text` (a mimeapps.list) with `id` first for `mime` in [Default
/// Applications]: put in front on the line for `mime` (the others stay as
/// fallbacks), else a line added at the end of that part, else the part
/// added at the end.
fn with_default(text: &str, mime: &str, id: &str) -> String {
    let line = format!("{mime}={id};");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == "[Default Applications]");
    let Some(start) = start else {
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("[Default Applications]".into());
        lines.push(line);
        return lines.join("\n") + "\n";
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |k| start + 1 + k);
    let key = |l: &str| l.split_once('=').map(|(k, _)| k.trim().to_string());
    if let Some(k) = (start + 1..end).find(|&k| key(&lines[k]).as_deref() == Some(mime)) {
        let (_, ids) = lines[k].split_once('=').unwrap_or_default();
        let others: Vec<&str> = ids
            .split(';')
            .map(str::trim)
            .filter(|x| !x.is_empty() && *x != id)
            .collect();
        lines[k] = others.iter().fold(line, |acc, x| format!("{acc}{x};"));
    } else {
        // After the part's last line, before blank lines that part it off.
        let mut at = end;
        while at > start + 1 && lines[at - 1].trim().is_empty() {
            at -= 1;
        }
        lines.insert(at, line);
    }
    lines.join("\n") + "\n"
}

pub fn launch(app: &App, files: &[PathBuf], cwd: &Path) {
    let mut cmd = command(&app.exec, files);
    if app.terminal {
        let mut t = crate::theme::configured_terminal(&crate::theme::config_home());
        t.push("-e".to_string());
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

    #[test]
    fn a_default_is_set_and_the_rest_kept() {
        let text = "# mine\n[Added Associations]\ntext/plain=a.desktop;\n\n[Default Applications]\nimage/png=old.desktop;\ntext/plain=b.desktop;\n\n[Removed Associations]\nx/y=z.desktop;\n";
        let out = with_default(text, "text/plain", "c.desktop");
        assert_eq!(
            out,
            text.replace("text/plain=b.desktop;", "text/plain=c.desktop;b.desktop;")
        );
        let again = with_default(&out, "text/plain", "b.desktop");
        assert_eq!(
            again,
            text.replace("text/plain=b.desktop;", "text/plain=b.desktop;c.desktop;")
        );
        let out = with_default(text, "video/mp4", "mpv.desktop");
        assert_eq!(
            out,
            text.replace(
                "text/plain=b.desktop;\n\n",
                "text/plain=b.desktop;\nvideo/mp4=mpv.desktop;\n\n"
            )
        );
        assert_eq!(
            with_default("", "a/b", "x.desktop"),
            "[Default Applications]\na/b=x.desktop;\n"
        );
        assert_eq!(
            with_default("[Added Associations]\na/b=y.desktop;\n", "a/b", "x.desktop"),
            "[Added Associations]\na/b=y.desktop;\n\n[Default Applications]\na/b=x.desktop;\n"
        );
        // The last part of the file.
        assert_eq!(
            with_default("[Default Applications]\nc/d=e;\n", "a/b", "x.desktop"),
            "[Default Applications]\nc/d=e;\na/b=x.desktop;\n"
        );
    }

    #[test]
    fn mimeapps_is_written_whole_and_through_a_link() {
        let d = std::env::temp_dir().join(format!("nirufm-mimeapps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("dotfiles")).unwrap();
        std::fs::write(d.join("dotfiles/mimeapps.list"), "[Default Applications]\n").unwrap();
        std::os::unix::fs::symlink(d.join("dotfiles/mimeapps.list"), d.join("mimeapps.list"))
            .unwrap();
        set_default(&d.join("mimeapps.list"), "text/plain", "ed.desktop").unwrap();
        assert!(std::fs::symlink_metadata(d.join("mimeapps.list"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(d.join("dotfiles/mimeapps.list")).unwrap(),
            "[Default Applications]\ntext/plain=ed.desktop;\n"
        );
        set_default(&d.join("new/mimeapps.list"), "a/b", "x.desktop").unwrap();
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            3,
            "no temporary files"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
