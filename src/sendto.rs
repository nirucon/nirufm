//! "Send to" in the menu: the command lines for NIRUSHARE devices, Proton
//! Drive through niruprodrive, and a new mail with the files attached.
//! Each program is only offered when it is on PATH, looked up when the
//! menu opens.
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A program and its arguments.
pub type Command = (&'static str, Vec<OsString>);

/// Where NIRUSHARE sends a file: one device or all of them.
pub enum Share<'a> {
    Host(&'a str),
    All,
}

/// `nirushare send --file F --to HOST` (or `--all`), one for each file.
pub fn nirushare(to: Share, files: &[PathBuf]) -> Vec<Command> {
    files
        .iter()
        .map(|f| {
            let mut a: Vec<OsString> = vec!["send".into()];
            match to {
                Share::Host(h) => {
                    a.push("--to".into());
                    a.push(h.into());
                }
                Share::All => a.push("--all".into()),
            }
            a.push("--file".into());
            a.push(f.into());
            ("nirushare", a)
        })
        .collect()
}

/// `niruprodrive upload PATH...`: to My files/NIRUX/Uploads in Proton Drive.
pub fn proton_upload(paths: &[PathBuf]) -> Command {
    let mut a: Vec<OsString> = vec!["upload".into()];
    a.extend(paths.iter().map(OsString::from));
    ("niruprodrive", a)
}

/// `niruprodrive link PATH`: a public link, which niruprodrive copies.
pub fn proton_link(path: &Path) -> Command {
    ("niruprodrive", vec!["link".into(), path.into()])
}

/// A new mail with `files` attached: through `xdg-email` (the desktop's
/// mail program) when there is one, else Thunderbird's compose window.
pub fn mail(files: &[PathBuf], xdg_email: bool, thunderbird: bool) -> Option<Command> {
    if files.is_empty() {
        return None;
    }
    if xdg_email {
        let mut a: Vec<OsString> = Vec::new();
        for f in files {
            a.push("--attach".into());
            a.push(f.into());
        }
        return Some(("xdg-email", a));
    }
    if thunderbird {
        // Thunderbird reads a comma-separated list of file URIs; commas and
        // quotes in names are percent-encoded by `file_uri`.
        let list: Vec<String> = files.iter().map(|f| crate::x::file_uri(f)).collect();
        return Some((
            "thunderbird",
            vec![
                "-compose".into(),
                format!("attachment='{}'", list.join(",")).into(),
            ],
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(c: &Command) -> Vec<String> {
        c.1.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn command_lines() {
        let files = vec![PathBuf::from("/h/a b.png"), PathBuf::from("/h/c,d.txt")];
        let v = nirushare(Share::Host("laptop"), &files);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "nirushare");
        assert_eq!(
            args(&v[0]),
            ["send", "--to", "laptop", "--file", "/h/a b.png"]
        );
        assert_eq!(
            args(&nirushare(Share::All, &files[1..])[0]),
            ["send", "--all", "--file", "/h/c,d.txt"]
        );
        let up = proton_upload(&files);
        assert_eq!(up.0, "niruprodrive");
        assert_eq!(args(&up), ["upload", "/h/a b.png", "/h/c,d.txt"]);
        assert_eq!(args(&proton_link(&files[0])), ["link", "/h/a b.png"]);
        let m = mail(&files, true, true).unwrap();
        assert_eq!(m.0, "xdg-email");
        assert_eq!(
            args(&m),
            ["--attach", "/h/a b.png", "--attach", "/h/c,d.txt"]
        );
        let t = mail(&files, false, true).unwrap();
        assert_eq!(t.0, "thunderbird");
        assert_eq!(
            args(&t),
            [
                "-compose",
                "attachment='file:///h/a%20b.png,file:///h/c%2Cd.txt'"
            ]
        );
        assert!(mail(&files, false, false).is_none());
        assert!(mail(&[], true, true).is_none());
    }
}
