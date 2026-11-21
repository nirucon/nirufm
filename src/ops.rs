//! File operations, run on a worker thread so the window never waits.
//!
//! Safety rules, the same as NIRUORG's:
//! - a copy is written to a hidden partial file and renamed into place only
//!   when complete, so an interrupted copy never looks like a finished file;
//! - a move across file systems deletes the source only after the copy;
//! - nothing is ever overwritten: a name that exists gets " (2)", " (3)"...;
//! - "delete" means the freedesktop trash, which other file managers share;
//!   permanent deletion is a separate, confirmed action.
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum Job {
    Copy {
        srcs: Vec<PathBuf>,
        dst: PathBuf,
    },
    Move {
        srcs: Vec<PathBuf>,
        dst: PathBuf,
    },
    Trash {
        paths: Vec<PathBuf>,
    },
    Delete {
        paths: Vec<PathBuf>,
    },
    Restore {
        pairs: Vec<(PathBuf, PathBuf, PathBuf)>,
    },
    Extract {
        archive: PathBuf,
        dst: PathBuf,
    },
    Compress {
        paths: Vec<PathBuf>,
        out: PathBuf,
    },
    Mount {
        dev: String,
    },
    Eject {
        dev: String,
    },
}

impl Job {
    pub fn verb(&self) -> &'static str {
        match self {
            Job::Copy { .. } => "Copying",
            Job::Move { .. } => "Moving",
            Job::Trash { .. } => "Moving to the trash",
            Job::Delete { .. } => "Deleting",
            Job::Restore { .. } => "Restoring",
            Job::Extract { .. } => "Unpacking",
            Job::Compress { .. } => "Packing",
            Job::Mount { .. } => "Mounting",
            Job::Eject { .. } => "Ejecting",
        }
    }
}

/// How to take an operation back.
#[derive(Clone, Debug)]
pub enum Undo {
    /// Move these (current, original) pairs back.
    MoveBack(Vec<(PathBuf, PathBuf)>),
    /// Put trashed files back: (file in trash, its .trashinfo, original).
    Untrash(Vec<(PathBuf, PathBuf, PathBuf)>),
    /// Copies and new items go to the trash.
    TrashCreated(Vec<PathBuf>),
}

pub enum Msg {
    Progress {
        done: u64,
        total: u64,
        current: String,
    },
    Done {
        summary: String,
        undo: Option<Undo>,
        errors: Vec<String>,
        created: Vec<PathBuf>,
        /// A folder to show when done (a drive that was just mounted).
        open: Option<PathBuf>,
    },
}

pub type Queued = (Job, Arc<AtomicBool>);

pub struct Worker {
    pub tx: Sender<Queued>,
}

/// Start the worker. `wake` is written to after every message so the event
/// loop's `poll` returns.
pub fn start(out: Sender<Msg>, wake: i32) -> Worker {
    let (tx, rx) = std::sync::mpsc::channel::<Queued>();
    std::thread::spawn(move || {
        for (job, cancel) in rx {
            let mut run = Run {
                out: &out,
                wake,
                done: 0,
                total: 0,
                cancel,
                errors: Vec::new(),
                last: std::time::Instant::now(),
            };
            let mount = matches!(job, Job::Mount { .. });
            let (summary, undo, created) = run.job(job);
            let open = if mount {
                created.first().cloned()
            } else {
                None
            };
            let _ = out.send(Msg::Done {
                summary,
                undo,
                errors: run.errors,
                created,
                open,
            });
            poke(wake);
        }
    });
    Worker { tx }
}

pub fn poke(fd: i32) {
    let one: u64 = 1;
    unsafe { libc::write(fd, &one as *const u64 as *const libc::c_void, 8) };
}

struct Run<'a> {
    out: &'a Sender<Msg>,
    wake: i32,
    done: u64,
    total: u64,
    cancel: Arc<AtomicBool>,
    errors: Vec<String>,
    last: std::time::Instant,
}

impl Run<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn progress(&mut self, current: &Path, force: bool) {
        if force || self.last.elapsed().as_millis() >= 80 {
            self.last = std::time::Instant::now();
            let _ = self.out.send(Msg::Progress {
                done: self.done,
                total: self.total,
                current: current
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            });
            poke(self.wake);
        }
    }

    fn fail(&mut self, what: &Path, e: impl std::fmt::Display) {
        self.errors.push(format!("{}: {e}", what.display()));
    }

    fn job(&mut self, job: Job) -> (String, Option<Undo>, Vec<PathBuf>) {
        match job {
            Job::Copy { srcs, dst } => {
                self.total = srcs.iter().map(|p| tree_size(p)).sum();
                let mut made = Vec::new();
                for s in &srcs {
                    if self.cancelled() {
                        break;
                    }
                    if dst.starts_with(s) {
                        self.fail(s, "cannot copy a folder into itself");
                        continue;
                    }
                    let target = free_name(&dst, s);
                    match self.copy_tree(s, &target) {
                        Ok(()) => made.push(target),
                        Err(e) => {
                            self.fail(s, e);
                            let _ = remove_tree(&target);
                        }
                    }
                }
                let n = made.len();
                (
                    format!("Copied {n} item{}", plural(n)),
                    Some(Undo::TrashCreated(made.clone())),
                    made,
                )
            }
            Job::Move { srcs, dst } => {
                self.total = srcs.len() as u64;
                let mut pairs = Vec::new();
                for s in &srcs {
                    if self.cancelled() {
                        break;
                    }
                    if s.parent() == Some(dst.as_path()) {
                        continue;
                    }
                    if dst.starts_with(s) {
                        self.fail(s, "cannot move a folder into itself");
                        continue;
                    }
                    let target = free_name(&dst, s);
                    match self.move_one(s, &target) {
                        Ok(()) => pairs.push((target, s.clone())),
                        Err(e) => self.fail(s, e),
                    }
                    self.done += 1;
                    self.progress(s, false);
                }
                let n = pairs.len();
                let created = pairs.iter().map(|(t, _)| t.clone()).collect();
                (
                    format!("Moved {n} item{}", plural(n)),
                    Some(Undo::MoveBack(pairs)),
                    created,
                )
            }
            Job::Trash { paths } => {
                self.total = paths.len() as u64;
                let mut done = Vec::new();
                for p in &paths {
                    if self.cancelled() {
                        break;
                    }
                    match trash(p) {
                        Ok(t) => done.push(t),
                        Err(e) => self.fail(p, e),
                    }
                    self.done += 1;
                    self.progress(p, false);
                }
                let n = done.len();
                (
                    format!("Moved {n} item{} to the trash", plural(n)),
                    Some(Undo::Untrash(done)),
                    Vec::new(),
                )
            }
            Job::Delete { paths } => {
                self.total = paths.len() as u64;
                let mut n = 0;
                for p in &paths {
                    match remove_tree(p) {
                        Ok(()) => n += 1,
                        Err(e) => self.fail(p, e),
                    }
                    self.done += 1;
                    self.progress(p, false);
                }
                (
                    format!("Deleted {n} item{} permanently", plural(n)),
                    None,
                    Vec::new(),
                )
            }
            Job::Restore { pairs } => {
                let mut n = 0;
                let mut created = Vec::new();
                for (file, info, orig) in &pairs {
                    let target = if orig.exists() {
                        free_name(orig.parent().unwrap_or(Path::new("/")), orig)
                    } else {
                        orig.clone()
                    };
                    if let Some(parent) = target.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    match self.move_one(file, &target) {
                        Ok(()) => {
                            let _ = fs::remove_file(info);
                            created.push(target);
                            n += 1;
                        }
                        Err(e) => self.fail(file, e),
                    }
                }
                (format!("Restored {n} item{}", plural(n)), None, created)
            }
            Job::Extract { archive, dst } => match crate::archive::extract(&archive, &dst) {
                Ok(made) => {
                    let name = made
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    (
                        format!("Unpacked to {name}"),
                        Some(Undo::TrashCreated(vec![made.clone()])),
                        vec![made],
                    )
                }
                Err(e) => {
                    self.fail(&archive, e);
                    ("Unpacking failed".into(), None, Vec::new())
                }
            },
            Job::Mount { dev } => match crate::devices::mount(&dev) {
                Ok(p) => (format!("Mounted {}", p.display()), None, vec![p]),
                Err(e) => {
                    self.fail(Path::new(&dev), e);
                    ("Mounting failed".into(), None, Vec::new())
                }
            },
            Job::Eject { dev } => match crate::devices::eject(&dev) {
                Ok(()) => ("Safe to remove".into(), None, Vec::new()),
                Err(e) => {
                    self.fail(Path::new(&dev), e);
                    ("Ejecting failed".into(), None, Vec::new())
                }
            },
            Job::Compress { paths, out } => match crate::archive::compress(&paths, &out) {
                Ok(()) => {
                    let name = out
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    (
                        format!("Packed {name}"),
                        Some(Undo::TrashCreated(vec![out.clone()])),
                        vec![out],
                    )
                }
                Err(e) => {
                    self.fail(&out, e);
                    ("Packing failed".into(), None, Vec::new())
                }
            },
        }
    }

    fn move_one(&mut self, s: &Path, target: &Path) -> std::io::Result<()> {
        match fs::rename(s, target) {
            Ok(()) => Ok(()),
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                // Another file system: copy completely, then remove.
                self.total += tree_size(s);
                self.copy_tree(s, target)?;
                if self.cancelled() {
                    let _ = remove_tree(target);
                    return Err(std::io::Error::other("cancelled"));
                }
                remove_tree(s)
            }
            Err(e) => Err(e),
        }
    }

    fn copy_tree(&mut self, s: &Path, d: &Path) -> std::io::Result<()> {
        let meta = fs::symlink_metadata(s)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(s)?, d)?;
        } else if ft.is_dir() {
            fs::create_dir(d)?;
            for e in fs::read_dir(s)? {
                if self.cancelled() {
                    return Err(std::io::Error::other("cancelled"));
                }
                let e = e?;
                self.copy_tree(&e.path(), &d.join(e.file_name()))?;
            }
            fs::set_permissions(d, fs::Permissions::from_mode(meta.mode() & 0o7777))?;
        } else if ft.is_file() {
            self.copy_file(s, d, &meta)?;
        }
        // Devices, sockets and pipes are skipped: they cannot be copied.
        Ok(())
    }

    fn copy_file(&mut self, s: &Path, d: &Path, meta: &fs::Metadata) -> std::io::Result<()> {
        let part = d.with_file_name(format!(
            ".{}.nirufm-part",
            d.file_name().unwrap_or_default().to_string_lossy()
        ));
        let result = (|| {
            let mut from = fs::File::open(s)?;
            let mut to = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&part)?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                if self.cancelled() {
                    return Err(std::io::Error::other("cancelled"));
                }
                let n = from.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                to.write_all(&buf[..n])?;
                self.done += n as u64;
                self.progress(s, false);
            }
            to.sync_all()?;
            fs::set_permissions(&part, fs::Permissions::from_mode(meta.mode() & 0o7777))?;
            set_mtime(&part, meta.mtime(), meta.mtime_nsec());
            // create_new on the final name would race; rename refuses nothing,
            // so check once more that the name is still free.
            if d.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "the name was taken meanwhile",
                ));
            }
            fs::rename(&part, d)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&part);
        }
        result
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn set_mtime(p: &Path, sec: i64, nsec: i64) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
        return;
    };
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        libc::timespec {
            tv_sec: sec as libc::time_t,
            tv_nsec: nsec as _,
        },
    ];
    unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
}

pub fn tree_size(p: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(p) else {
        return 0;
    };
    if m.is_dir() {
        fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        m.len()
    }
}

pub fn remove_tree(p: &Path) -> std::io::Result<()> {
    let m = fs::symlink_metadata(p)?;
    if m.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// `dst/name`, or "name (2).ext", "name (3).ext"... when that exists.
pub fn free_name(dst: &Path, src: &Path) -> PathBuf {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".into());
    let first = dst.join(&name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let is_dir = fs::symlink_metadata(src).is_ok_and(|m| m.is_dir());
    let (stem, ext) = split_ext(&name, is_dir);
    for k in 2.. {
        let p = dst.join(format!("{stem} ({k}){ext}"));
        if fs::symlink_metadata(&p).is_err() {
            return p;
        }
    }
    unreachable!()
}

/// "photo.tar.gz" -> ("photo", ".tar.gz"); folders and dotfiles keep it all.
pub fn split_ext(name: &str, is_dir: bool) -> (String, String) {
    if is_dir || name.starts_with('.') && name.matches('.').count() == 1 {
        return (name.to_string(), String::new());
    }
    for double in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if name.to_lowercase().ends_with(double) && name.len() > double.len() {
            let cut = name.len() - double.len();
            return (name[..cut].to_string(), name[cut..].to_string());
        }
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

// ------------------------------------------------------------------ trash

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::theme::home().join(".local/share"))
}

/// The trash for `p`: the home trash on the home file system, otherwise
/// `$topdir/.Trash-$uid` of the drive it is on (freedesktop trash spec).
fn trash_dir_for(p: &Path) -> std::io::Result<PathBuf> {
    let home_trash = data_home().join("Trash");
    let dev = fs::symlink_metadata(p)?.dev();
    let _ = fs::create_dir_all(home_trash.join("files"));
    let _ = fs::create_dir_all(home_trash.join("info"));
    if fs::metadata(&home_trash).map(|m| m.dev()).ok() == Some(dev) {
        return Ok(home_trash);
    }
    // Walk up to the mount point (the last ancestor on the same device).
    let mut top = p.to_path_buf();
    while let Some(parent) = top.parent() {
        if fs::metadata(parent).map(|m| m.dev()).ok() != Some(dev) {
            break;
        }
        top = parent.to_path_buf();
    }
    let uid = unsafe { libc::getuid() };
    let t = top.join(format!(".Trash-{uid}"));
    fs::create_dir_all(t.join("files"))?;
    fs::create_dir_all(t.join("info"))?;
    let _ = fs::set_permissions(&t, fs::Permissions::from_mode(0o700));
    Ok(t)
}

fn encode_path(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::new();
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

pub fn decode_path(s: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Move `p` to the trash. Returns (file in trash, info file, original).
pub fn trash(p: &Path) -> std::io::Result<(PathBuf, PathBuf, PathBuf)> {
    let orig = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()?.join(p)
    };
    let dir = trash_dir_for(&orig)?;
    let name = orig
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".into());
    let (stem, ext) = split_ext(&name, orig.is_dir());
    // The info file is created first and exclusively: it reserves the name.
    let mut k = 1;
    let (info, file) = loop {
        let n = if k == 1 {
            name.clone()
        } else {
            format!("{stem} ({k}){ext}")
        };
        let info = dir.join("info").join(format!("{n}.trashinfo"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info)
        {
            Ok(mut f) => {
                let home_trash = dir == data_home().join("Trash");
                let shown = if home_trash {
                    encode_path(&orig)
                } else {
                    let top = dir.parent().unwrap_or(Path::new("/"));
                    encode_path(orig.strip_prefix(top).unwrap_or(&orig))
                };
                writeln!(f, "[Trash Info]\nPath={shown}\nDeletionDate={}", now_iso())?;
                break (info, dir.join("files").join(n));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => k += 1,
            Err(e) => return Err(e),
        }
    };
    if let Err(e) = fs::rename(&orig, &file) {
        let _ = fs::remove_file(&info);
        return Err(e);
    }
    Ok((file, info, orig))
}

fn now_iso() -> String {
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// The original path of an item in the home trash, from its .trashinfo.
pub fn trash_origin(file: &Path) -> Option<(PathBuf, PathBuf)> {
    let dir = file.parent()?.parent()?;
    let info = dir
        .join("info")
        .join(format!("{}.trashinfo", file.file_name()?.to_string_lossy()));
    let text = fs::read_to_string(&info).ok()?;
    let raw = text.lines().find_map(|l| l.strip_prefix("Path="))?;
    let p = decode_path(raw);
    let p = if p.is_absolute() {
        p
    } else {
        dir.parent()?.join(p)
    };
    Some((info, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nirufm-ops-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(job: Job) -> (String, Option<Undo>, Vec<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK) };
        let w = start(tx, fd);
        w.tx.send((job, Arc::new(AtomicBool::new(false)))).unwrap();
        loop {
            if let Msg::Done {
                summary,
                undo,
                errors,
                ..
            } = rx.recv().unwrap()
            {
                unsafe { libc::close(fd) };
                return (summary, undo, errors);
            }
        }
    }

    #[test]
    fn names_never_collide() {
        let d = tmp("names");
        fs::write(d.join("a.txt"), "1").unwrap();
        fs::write(d.join("a (2).txt"), "2").unwrap();
        assert_eq!(free_name(&d, &d.join("a.txt")), d.join("a (3).txt"));
        assert_eq!(split_ext("x.tar.gz", false), ("x".into(), ".tar.gz".into()));
        assert_eq!(
            split_ext(".bashrc", false),
            (".bashrc".into(), String::new())
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn copy_keeps_content_and_mode_and_never_overwrites() {
        let d = tmp("copy");
        fs::create_dir_all(d.join("src/sub")).unwrap();
        fs::write(d.join("src/sub/f"), "hello").unwrap();
        fs::set_permissions(d.join("src/sub/f"), fs::Permissions::from_mode(0o750)).unwrap();
        fs::create_dir(d.join("dst")).unwrap();
        fs::create_dir(d.join("dst/src")).unwrap();
        let (_, undo, errors) = run(Job::Copy {
            srcs: vec![d.join("src")],
            dst: d.join("dst"),
        });
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            fs::read_to_string(d.join("dst/src (2)/sub/f")).unwrap(),
            "hello"
        );
        assert_eq!(
            fs::metadata(d.join("dst/src (2)/sub/f")).unwrap().mode() & 0o777,
            0o750
        );
        assert!(matches!(undo, Some(Undo::TrashCreated(v)) if v == vec![d.join("dst/src (2)")]));
        let leftovers: Vec<_> = fs::read_dir(d.join("dst/src (2)/sub"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("nirufm-part"))
            .collect();
        assert!(leftovers.is_empty());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_folder_cannot_be_copied_or_moved_into_itself() {
        let d = tmp("self");
        fs::create_dir_all(d.join("a/b")).unwrap();
        let (_, _, errors) = run(Job::Copy {
            srcs: vec![d.join("a")],
            dst: d.join("a/b"),
        });
        assert_eq!(errors.len(), 1);
        let (_, _, errors) = run(Job::Move {
            srcs: vec![d.join("a")],
            dst: d.join("a/b"),
        });
        assert_eq!(errors.len(), 1);
        assert!(d.join("a/b").is_dir());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn trash_writes_trashinfo_and_restore_puts_it_back() {
        let d = tmp("trash");
        let data = d.join("data");
        std::env::set_var("XDG_DATA_HOME", &data);
        fs::write(d.join("note 1.txt"), "x").unwrap();
        let (file, info, orig) = trash(&d.join("note 1.txt")).unwrap();
        assert!(!orig.exists() && file.exists());
        let text = fs::read_to_string(&info).unwrap();
        assert!(
            text.contains("Path=") && text.contains("note%201.txt"),
            "{text}"
        );
        assert_eq!(trash_origin(&file).unwrap().1, orig);
        let (_, _, errors) = run(Job::Restore {
            pairs: vec![(file.clone(), info.clone(), orig.clone())],
        });
        assert!(errors.is_empty());
        assert!(orig.exists() && !file.exists() && !info.exists());
        fs::remove_dir_all(d).unwrap();
    }
}
