//! Packing and unpacking through the archive tools the system has:
//! bsdtar (libarchive: zip, 7z, tar.*, rar, iso and more), then GNU tar,
//! unzip, 7z and zip. Unpacking goes to a hidden temporary folder first;
//! a single top-level item is then moved out, so "photos.zip" with a
//! "photos" folder inside does not end up as photos/photos.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn have(tool: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file()))
        .unwrap_or(false)
}

const ARCHIVE_EXT: [&str; 16] = [
    ".zip", ".7z", ".rar", ".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tar.xz", ".txz",
    ".tar.zst", ".gz", ".bz2", ".xz", ".iso", ".cpio",
];

pub fn is_archive(name: &str) -> bool {
    let l = name.to_lowercase();
    ARCHIVE_EXT.iter().any(|e| l.ends_with(e))
}

fn run(mut c: Command) -> Result<(), String> {
    let out = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr);
        Err(msg.lines().last().unwrap_or("failed").trim().to_string())
    }
}

/// Unpack `archive` into `dst`. Returns the created file or folder.
pub fn extract(archive: &Path, dst: &Path) -> Result<PathBuf, String> {
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_lowercase();
    let tmp = dst.join(format!(".nirufm-unpack-{}", std::process::id()));
    std::fs::create_dir(&tmp).map_err(|e| e.to_string())?;
    let single_file =
        [".gz", ".bz2", ".xz"].iter().any(|e| lower.ends_with(e)) && !lower.contains(".tar.");
    let result = if single_file {
        // A compressed single file (notes.txt.gz): decompress beside it.
        let (tool, flag) = if lower.ends_with(".gz") {
            ("gzip", "-dc")
        } else if lower.ends_with(".bz2") {
            ("bzip2", "-dc")
        } else {
            ("xz", "-dc")
        };
        let stem = &name[..name.rfind('.').unwrap_or(name.len())];
        std::fs::File::create(tmp.join(stem))
            .map_err(|e| e.to_string())
            .and_then(|f| {
                let st = Command::new(tool)
                    .arg(flag)
                    .arg(archive)
                    .stdout(f)
                    .stderr(Stdio::null())
                    .status()
                    .map_err(|e| e.to_string())?;
                if st.success() {
                    Ok(())
                } else {
                    Err(format!("{tool} failed"))
                }
            })
    } else if have("bsdtar") {
        let mut c = Command::new("bsdtar");
        c.arg("-xf").arg(archive).arg("-C").arg(&tmp);
        run(c)
    } else if lower.ends_with(".zip") && have("unzip") {
        let mut c = Command::new("unzip");
        c.arg("-q").arg(archive).arg("-d").arg(&tmp);
        run(c)
    } else if (lower.ends_with(".7z") || lower.ends_with(".rar") || lower.ends_with(".zip"))
        && have("7z")
    {
        let mut c = Command::new("7z");
        c.arg("x")
            .arg("-y")
            .arg(format!("-o{}", tmp.display()))
            .arg(archive);
        run(c)
    } else if have("tar") {
        let mut c = Command::new("tar");
        c.arg("-xf").arg(archive).arg("-C").arg(&tmp);
        run(c)
    } else {
        Err("no tool to unpack this archive (install libarchive-tools)".into())
    };
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let items: Vec<PathBuf> = std::fs::read_dir(&tmp)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .collect();
    let made = if items.len() == 1 {
        let target = crate::ops::free_name(dst, &items[0]);
        std::fs::rename(&items[0], &target).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir(&tmp);
        target
    } else {
        let (stem, _) = crate::ops::split_ext(&name, false);
        let target = crate::ops::free_name(dst, Path::new(&stem));
        std::fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
        target
    };
    Ok(made)
}

/// Pack `paths` (all in one folder) into `out`; the format follows its
/// extension (.zip, .tar.gz, .tar.xz, .7z...).
pub fn compress(paths: &[PathBuf], out: &Path) -> Result<(), String> {
    let dir = paths
        .first()
        .and_then(|p| p.parent())
        .ok_or("nothing to pack")?;
    let names: Vec<_> = paths.iter().filter_map(|p| p.file_name()).collect();
    let lower = out.to_string_lossy().to_lowercase();
    let part = out.with_file_name(format!(
        ".{}.nirufm-part",
        out.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = if lower.ends_with(".zip") && have("zip") {
        let mut c = Command::new("zip");
        c.current_dir(dir)
            .arg("-r")
            .arg("-q")
            .arg("-y")
            .arg(&part)
            .arg("--")
            .args(&names);
        // zip appends .zip to a name without it; the part name ends in -part.
        let r = run(c);
        let zipped = PathBuf::from(format!("{}.zip", part.display()));
        if zipped.exists() {
            let _ = std::fs::rename(&zipped, &part);
        }
        r
    } else if have("bsdtar") {
        let mut c = Command::new("bsdtar");
        let fmt = if lower.ends_with(".zip") {
            vec!["--format", "zip"]
        } else if lower.ends_with(".7z") {
            vec!["--format", "7zip"]
        } else if lower.ends_with(".tar.xz") {
            vec!["--format", "pax", "-J"]
        } else if lower.ends_with(".tar.zst") {
            vec!["--format", "pax", "--zstd"]
        } else if lower.ends_with(".tar.bz2") {
            vec!["--format", "pax", "-j"]
        } else {
            vec!["--format", "pax", "-z"]
        };
        c.current_dir(dir)
            .arg("-c")
            .args(fmt)
            .arg("-f")
            .arg(&part)
            .arg("--")
            .args(&names);
        run(c)
    } else if have("tar") && lower.contains(".tar") {
        let flag = if lower.ends_with(".xz") {
            "-J"
        } else if lower.ends_with(".bz2") {
            "-j"
        } else if lower.ends_with(".zst") {
            "--zstd"
        } else {
            "-z"
        };
        let mut c = Command::new("tar");
        c.current_dir(dir)
            .arg("-c")
            .arg(flag)
            .arg("-f")
            .arg(&part)
            .arg("--")
            .args(&names);
        run(c)
    } else {
        Err("no tool to pack this format (install zip or libarchive-tools)".into())
    };
    match result {
        Ok(()) => std::fs::rename(&part, out).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_and_unpack_round_trip_without_nesting() {
        if !have("tar") {
            return;
        }
        let d = std::env::temp_dir().join(format!("nirufm-arch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("photos/sub")).unwrap();
        std::fs::write(d.join("photos/sub/a.txt"), "a").unwrap();
        let out = d.join("photos.tar.gz");
        compress(&[d.join("photos")], &out).unwrap();
        assert!(out.is_file());
        let made = extract(&out, &d).unwrap();
        // One top-level folder: it is not nested, and the name is free.
        assert_eq!(made, d.join("photos (2)"));
        assert_eq!(
            std::fs::read_to_string(made.join("sub/a.txt")).unwrap(),
            "a"
        );
        assert!(is_archive("X.TAR.GZ") && !is_archive("notes.txt"));
        std::fs::remove_dir_all(d).unwrap();
    }
}
