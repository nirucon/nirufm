//! Changing who may read, write and run (or, for a folder, open) an item,
//! for the properties sheet.
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The bit of `who` (0 owner, 1 group, 2 others) and `what` (0 read,
/// 1 write, 2 run or open).
pub fn bit(who: usize, what: usize) -> u32 {
    (0o4 >> what) << (3 * (2 - who))
}

/// "Executable" in one click: on gives run to the owner and to everyone
/// else who may read it, off takes it from all.
pub fn executable(mode: u32, on: bool) -> u32 {
    if !on {
        return mode & !0o111;
    }
    let mut m = mode | 0o100;
    for who in 1..3 {
        if mode & bit(who, 0) != 0 {
            m |= bit(who, 2);
        }
    }
    m
}

/// Set `mode` (the permission bits) on `path`. Returns the mode it has
/// afterwards, or what went wrong in words.
pub fn apply(path: &Path, mode: u32) -> Result<u32, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let before = std::fs::metadata(path).map_err(|e| error(&e, &name))?;
    let keep = before.permissions().mode() & !0o7777;
    std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(keep | (mode & 0o7777)),
    )
    .map_err(|e| error(&e, &name))?;
    let now = std::fs::metadata(path)
        .map_err(|e| error(&e, &name))?
        .permissions()
        .mode()
        & 0o7777;
    // FAT, exFAT and Windows shares accept the change and forget it.
    if now != mode & 0o7777 {
        return Err("This drive does not keep permissions (FAT, exFAT or a Windows share?)".into());
    }
    Ok(now)
}

/// A failed change in plain words.
pub fn error(e: &std::io::Error, name: &str) -> String {
    match e.raw_os_error() {
        Some(libc::EPERM | libc::EACCES) => {
            format!("Only the owner of \"{name}\" (or an administrator) can change who may use it")
        }
        Some(libc::EROFS) => format!("\"{name}\" is on a read-only drive"),
        Some(libc::ENOENT) => format!("\"{name}\" is no longer there"),
        _ => format!("Could not change the permissions of \"{name}\": {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_and_the_executable_click() {
        assert_eq!(bit(0, 0), 0o400);
        assert_eq!(bit(0, 2), 0o100);
        assert_eq!(bit(1, 1), 0o020);
        assert_eq!(bit(2, 2), 0o001);
        assert_eq!(executable(0o644, true), 0o755);
        assert_eq!(executable(0o640, true), 0o750);
        assert_eq!(executable(0o600, true), 0o700);
        assert_eq!(executable(0o4755, false), 0o4644);
    }

    #[test]
    fn modes_are_applied_and_errors_worded() {
        let d = std::env::temp_dir().join(format!("nirufm-perms-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("run.sh");
        std::fs::write(&f, "").unwrap();
        assert_eq!(apply(&f, 0o700), Ok(0o700));
        assert_eq!(apply(&f, 0o644), Ok(0o644));
        let e = apply(&d.join("gone"), 0o644).unwrap_err();
        assert!(e.contains("no longer there"), "{e}");
        let e = std::io::Error::from_raw_os_error(libc::EPERM);
        assert!(error(&e, "x").starts_with("Only the owner"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
