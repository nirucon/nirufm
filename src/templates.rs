//! New items: an empty file, or a copy of a template from the templates
//! folder (`XDG_TEMPLATES_DIR` in user-dirs.dirs, else ~/Templates), as in
//! other file managers.
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// How many templates the menu lists; the rest would make it too long.
pub const MAX: usize = 10;

/// The templates folder. None when user-dirs.dirs sets it to the home
/// folder itself, which is how that file says "no templates folder".
pub fn dir() -> Option<PathBuf> {
    let home = crate::theme::home();
    let set = crate::places::user_dirs(&home)
        .into_iter()
        .find(|(k, _)| k == "TEMPLATES")
        .map(|(_, p)| p);
    match set {
        Some(p) if p == home || p.as_os_str().is_empty() => None,
        Some(p) => Some(p),
        None => Some(home.join("Templates")),
    }
}

/// The files in `dir`, by name; hidden ones and folders are left out.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        // A link to a file counts, so templates can live elsewhere.
        .filter(|e| e.path().is_file())
        .map(|e| e.path())
        .collect();
    v.sort_by(|a, b| {
        crate::dir::natural(
            &a.file_name().unwrap_or_default().to_string_lossy(),
            &b.file_name().unwrap_or_default().to_string_lossy(),
        )
    });
    v
}

/// A new empty file "New file" in `dir` (or "New file (2)"...).
pub fn create_empty(dir: &Path) -> std::io::Result<PathBuf> {
    create(dir, Path::new("New file"), b"", 0o666)
}

/// A copy of `template` in `dir`, under its name or a free one beside it.
/// It keeps the template's permissions (a script stays executable).
pub fn create_from(template: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let data = fs::read(template)?;
    let mode = fs::metadata(template)?.permissions().mode() & 0o777;
    create(dir, template, &data, mode)
}

/// Write `data` to a free name for `name` in `dir`. The file is created
/// exclusively, so it never replaces one made meanwhile by someone else.
fn create(dir: &Path, name: &Path, data: &[u8], mode: u32) -> std::io::Result<PathBuf> {
    for _ in 0..5 {
        let p = crate::ops::free_name(dir, name);
        let f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&p);
        match f {
            Ok(mut f) => {
                if let Err(e) = f.write_all(data).and_then(|()| f.sync_all()) {
                    let _ = fs::remove_file(&p);
                    return Err(e);
                }
                return Ok(p);
            }
            // Taken between the look and the create: look again.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

/// A failed creation in words for the status line.
pub fn error(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EACCES | libc::EPERM) => "No permission to create files in this folder".into(),
        Some(libc::EROFS) => "This drive is read-only".into(),
        Some(libc::ENOSPC) => "The drive is full".into(),
        _ => format!("Could not create the file: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nirufm-tpl-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn empty_files_get_free_names() {
        let d = tmp("empty");
        assert_eq!(create_empty(&d).unwrap(), d.join("New file"));
        assert_eq!(create_empty(&d).unwrap(), d.join("New file (2)"));
        assert_eq!(fs::metadata(d.join("New file")).unwrap().len(), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn templates_are_listed_and_copied_with_their_mode() {
        let d = tmp("tpl");
        let t = d.join("Templates");
        fs::create_dir_all(t.join("sub")).unwrap();
        fs::write(t.join("Letter.odt"), "letter").unwrap();
        fs::write(t.join("script 10.sh"), "#!/bin/sh\n").unwrap();
        fs::write(t.join("script 9.sh"), "#!/bin/sh\n").unwrap();
        fs::write(t.join(".hidden"), "").unwrap();
        fs::set_permissions(t.join("script 9.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        let names: Vec<String> = list(&t)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["Letter.odt", "script 9.sh", "script 10.sh"]);
        let out = d.join("out");
        fs::create_dir(&out).unwrap();
        let p = create_from(&t.join("script 9.sh"), &out).unwrap();
        assert_eq!(p, out.join("script 9.sh"));
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o100,
            0o100
        );
        let p = create_from(&t.join("Letter.odt"), &out).unwrap();
        let p2 = create_from(&t.join("Letter.odt"), &out).unwrap();
        assert_eq!(p2, out.join("Letter (2).odt"));
        assert_eq!(fs::read_to_string(p).unwrap(), "letter");
        assert!(list(&d.join("none")).is_empty());
        let _ = fs::remove_dir_all(&d);
    }
}
