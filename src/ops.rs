//! File operations, run on a worker thread so the window never waits.
//!
//! Safety rules, the same as NIRUORG's:
//! - a copy is written to a hidden partial file and renamed into place only
//!   when complete, so an interrupted copy never looks like a finished file;
//! - a move across file systems deletes the source only after the copy;
//! - nothing is overwritten unasked: when a name exists the user chooses to
//!   replace, skip, keep both (" (2)", " (3)"...) or merge two folders, and
//!   a replaced item goes to the trash only once its successor is complete;
//! - "delete" means the freedesktop trash, which other file managers share;
//!   permanent deletion is a separate, confirmed action.
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
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
    Connect {
        uri: String,
        login: Option<(String, String)>,
    },
    Disconnect {
        uri: String,
    },
    /// Take an operation back (or, with what that returned, do it again).
    Revert {
        undo: Undo,
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
            Job::Connect { .. } => "Connecting",
            Job::Disconnect { .. } => "Disconnecting",
            Job::Revert { .. } => "Undoing",
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
    /// Several of these, taken back in this order.
    Steps(Vec<Undo>),
}

impl Undo {
    /// Several steps as one, leaving out the empty ones.
    fn steps(v: Vec<Undo>) -> Option<Undo> {
        let mut v: Vec<Undo> = v.into_iter().filter(|u| !u.is_empty()).collect();
        match v.len() {
            0 => None,
            1 => v.pop(),
            _ => Some(Undo::Steps(v)),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Undo::MoveBack(v) => v.is_empty(),
            Undo::Untrash(v) => v.is_empty(),
            Undo::TrashCreated(v) => v.is_empty(),
            Undo::Steps(v) => v.iter().all(Undo::is_empty),
        }
    }
}

/// What to do when an item of the same name is where another goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Replace,
    Skip,
    KeepBoth,
    /// Folder onto folder: the contents go in alongside, asking again for
    /// each name inside that exists too.
    Merge,
}

/// The user's answer to `Msg::Ask`; `all` applies it to the conflicts
/// still to come in this job (Merge only to folders).
#[derive(Clone, Copy, Debug)]
pub struct Answer {
    pub choice: Choice,
    pub all: bool,
}

/// One side of a conflict, to choose by.
#[derive(Clone, Debug)]
pub struct Side {
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

impl Side {
    fn of(p: &Path) -> Side {
        let m = fs::symlink_metadata(p).ok();
        Side {
            is_dir: m.as_ref().is_some_and(|m| m.is_dir()),
            size: m.as_ref().map_or(0, |m| m.len()),
            mtime: m.as_ref().map_or(0, |m| m.mtime()),
        }
    }
}

pub enum Msg {
    /// `name` exists in `folder`: the worker waits for an `Answer`.
    Ask {
        name: String,
        folder: PathBuf,
        existing: Side,
        incoming: Side,
    },
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
        /// The server at this URI wants a user name and password.
        login: Option<String>,
    },
}

pub type Queued = (Job, Arc<AtomicBool>);

pub struct Worker {
    pub tx: Sender<Queued>,
    /// Answers to `Msg::Ask`.
    pub answer: Sender<Answer>,
}

/// Start the worker. `wake` is written to after every message so the event
/// loop's `poll` returns.
pub fn start(out: Sender<Msg>, wake: i32) -> Worker {
    let (tx, rx) = std::sync::mpsc::channel::<Queued>();
    let (answer, answers) = std::sync::mpsc::channel::<Answer>();
    std::thread::spawn(move || {
        for (job, cancel) in rx {
            let mut run = Run {
                out: &out,
                answers: &answers,
                wake,
                done: 0,
                total: 0,
                cancel,
                errors: Vec::new(),
                last: std::time::Instant::now(),
                needs_login: false,
                all_files: None,
                all_dirs: None,
            };
            let mount = matches!(job, Job::Mount { .. } | Job::Connect { .. });
            let wants_login = match &job {
                Job::Connect { uri, .. } => Some(uri.clone()),
                _ => None,
            };
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
                login: wants_login.filter(|_| run.needs_login),
            });
            poke(wake);
        }
    });
    Worker { tx, answer }
}

pub fn poke(fd: i32) {
    let one: u64 = 1;
    unsafe { libc::write(fd, &one as *const u64 as *const libc::c_void, 8) };
}

struct Run<'a> {
    out: &'a Sender<Msg>,
    answers: &'a Receiver<Answer>,
    wake: i32,
    done: u64,
    total: u64,
    cancel: Arc<AtomicBool>,
    errors: Vec<String>,
    last: std::time::Instant,
    needs_login: bool,
    /// "Apply to all" answers for the rest of the job: for files (and
    /// anything that is not folder onto folder), and for folders.
    all_files: Option<Choice>,
    all_dirs: Option<Choice>,
}

/// What a copy or move did, to report and to undo.
#[derive(Default)]
struct Moved {
    /// Copies made (top level, or inside a merged folder).
    made: Vec<PathBuf>,
    /// Moved items: (now, where they were).
    moved: Vec<(PathBuf, PathBuf)>,
    /// Items replaced, now in the trash.
    replaced: Vec<(PathBuf, PathBuf, PathBuf)>,
    skipped: usize,
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
            Job::Copy { srcs, dst } => self.transfer(&srcs, &dst, false),
            Job::Move { srcs, dst } => self.transfer(&srcs, &dst, true),
            Job::Revert { undo } => {
                let back = self.revert(undo);
                let created = back.as_ref().map(touched).unwrap_or_default();
                ("Undone".into(), back, created)
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
                let mut created = Vec::new();
                for (file, info, orig) in &pairs {
                    match self.restore_one(file, info, orig) {
                        Ok(target) => created.push(target),
                        Err(e) => self.fail(file, e),
                    }
                }
                let n = created.len();
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
            Job::Connect { uri, login } => {
                let l = login.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
                match crate::remote::connect(&uri, l) {
                    Ok(p) => (format!("Connected to {uri}"), None, vec![p]),
                    Err(crate::remote::ConnectError::NeedsLogin) => {
                        self.needs_login = true;
                        ("The server wants a login".into(), None, Vec::new())
                    }
                    Err(crate::remote::ConnectError::Failed(e)) => {
                        self.errors.push(format!("{uri}: {e}"));
                        ("Connecting failed".into(), None, Vec::new())
                    }
                }
            }
            Job::Disconnect { uri } => match crate::remote::disconnect(&uri) {
                Ok(()) => ("Disconnected".into(), None, Vec::new()),
                Err(e) => {
                    self.errors.push(format!("{uri}: {e}"));
                    ("Disconnecting failed".into(), None, Vec::new())
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

    /// Copy or move `srcs` into `dst`, asking about each name that exists.
    fn transfer(
        &mut self,
        srcs: &[PathBuf],
        dst: &Path,
        mov: bool,
    ) -> (String, Option<Undo>, Vec<PathBuf>) {
        self.total = if mov {
            srcs.len() as u64
        } else {
            srcs.iter().map(|p| tree_size(p)).sum()
        };
        let mut m = Moved::default();
        for s in srcs {
            if self.cancelled() {
                break;
            }
            if mov && s.parent() == Some(dst) {
                continue;
            }
            if dst.starts_with(s) {
                let verb = if mov { "move" } else { "copy" };
                self.fail(s, format!("cannot {verb} a folder into itself"));
                continue;
            }
            let Some(name) = s.file_name() else {
                self.fail(s, "cannot copy or move this");
                continue;
            };
            self.place(s, &dst.join(name), mov, &mut m);
            if mov {
                self.done += 1;
                self.progress(s, false);
            }
        }
        let n = if mov { m.moved.len() } else { m.made.len() };
        let mut notes = Vec::new();
        if !m.replaced.is_empty() {
            notes.push(format!("{} replaced", m.replaced.len()));
        }
        if m.skipped > 0 {
            notes.push(format!("{} skipped", m.skipped));
        }
        let summary = format!(
            "{} {n} item{}{}",
            if mov { "Moved" } else { "Copied" },
            plural(n),
            if notes.is_empty() {
                String::new()
            } else {
                format!(" ({})", notes.join(", "))
            }
        );
        let (undo, created) = if mov {
            let created = m.moved.iter().map(|(t, _)| t.clone()).collect();
            (Undo::MoveBack(m.moved), created)
        } else {
            (Undo::TrashCreated(m.made.clone()), m.made)
        };
        (
            summary,
            Undo::steps(vec![undo, Undo::Untrash(m.replaced)]),
            created,
        )
    }

    /// `s` to `t`: straight there when the name is free, else as the user
    /// chooses.
    fn place(&mut self, s: &Path, t: &Path, mov: bool, m: &mut Moved) {
        let dir = t.parent().unwrap_or(Path::new("/"));
        if fs::symlink_metadata(t).is_err() {
            return self.put(s, t, mov, m);
        }
        if same_file(s, t) {
            // Pasted where it is: a copy goes beside it, a move is done.
            if !mov {
                let t = free_name(dir, s);
                self.put(s, &t, mov, m);
            }
            return;
        }
        let (existing, incoming) = (Side::of(t), Side::of(s));
        let folders = existing.is_dir && incoming.is_dir;
        match self.choose(t, existing, incoming) {
            Choice::Skip => m.skipped += 1,
            Choice::KeepBoth => {
                let t = free_name(dir, s);
                self.put(s, &t, mov, m);
            }
            Choice::Merge if folders => self.merge(s, t, mov, m),
            // Merge is offered only for two folders.
            Choice::Merge => m.skipped += 1,
            Choice::Replace => self.replace(s, t, mov, m),
        }
    }

    /// `s` to the free name `t`.
    fn put(&mut self, s: &Path, t: &Path, mov: bool, m: &mut Moved) {
        let result = if mov {
            self.move_one(s, t)
        } else {
            self.copy_tree(s, t)
        };
        match result {
            Ok(()) if mov => m.moved.push((t.to_path_buf(), s.to_path_buf())),
            Ok(()) => m.made.push(t.to_path_buf()),
            Err(e) => {
                // What was made of it goes, unless the name was taken by
                // someone else meanwhile.
                if !mov && e.kind() != std::io::ErrorKind::AlreadyExists {
                    let _ = remove_tree(t);
                }
                self.fail(s, e);
            }
        }
    }

    /// The choice for a name that exists: remembered from "apply to all",
    /// else asked. The window stays live meanwhile; only this job waits.
    fn choose(&mut self, t: &Path, existing: Side, incoming: Side) -> Choice {
        let folders = existing.is_dir && incoming.is_dir;
        if let Some(c) = if folders {
            self.all_dirs
        } else {
            self.all_files
        } {
            return c;
        }
        if self.cancelled() {
            return Choice::Skip;
        }
        // An answer left over from an earlier question is not for this one.
        while self.answers.try_recv().is_ok() {}
        let _ = self.out.send(Msg::Ask {
            name: t
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            folder: t.parent().map(Path::to_path_buf).unwrap_or_default(),
            existing,
            incoming,
        });
        poke(self.wake);
        let a = self.answers.recv().unwrap_or(Answer {
            choice: Choice::Skip,
            all: true,
        });
        if a.all {
            if a.choice == Choice::Merge {
                self.all_dirs = Some(Choice::Merge);
            } else {
                self.all_files = Some(a.choice);
                self.all_dirs.get_or_insert(a.choice);
            }
        }
        // A pause to answer is not part of the copy's speed.
        self.last = std::time::Instant::now();
        a.choice
    }

    /// Put `s` in the place of `t`. The successor is made complete first
    /// (moved within the drive, else copied under a hidden name beside
    /// `t`); only then does `t` go to the trash, so it can come back.
    fn replace(&mut self, s: &Path, t: &Path, mov: bool, m: &mut Moved) {
        if s.starts_with(t) {
            self.fail(s, "cannot replace a folder with something inside it");
            return;
        }
        let dir = t.parent().unwrap_or(Path::new("/"));
        let dev = |p: &Path| fs::metadata(p).map(|m| m.dev()).ok();
        let in_place = mov && dev(s).is_some() && dev(s) == dev(dir);
        let temp = dir.join(format!(
            ".{}.nirufm-new-{}",
            t.file_name().unwrap_or_default().to_string_lossy(),
            std::process::id()
        ));
        if !in_place {
            if mov {
                self.total += tree_size(s);
            }
            if let Err(e) = self.copy_tree(s, &temp) {
                let _ = remove_tree(&temp);
                return self.fail(s, e);
            }
        }
        let new = if in_place { s } else { temp.as_path() };
        let old = match trash(t) {
            Ok(old) => old,
            Err(e) => {
                if !in_place {
                    let _ = remove_tree(&temp);
                }
                return self.fail(t, format!("kept, it could not go to the trash ({e})"));
            }
        };
        if let Err(e) = rename_noreplace(new, t) {
            // The old one comes back from the trash.
            if rename_noreplace(&old.0, t).is_ok() {
                let _ = fs::remove_file(&old.1);
            }
            if !in_place {
                let _ = remove_tree(&temp);
            }
            return self.fail(s, e);
        }
        m.replaced.push(old);
        if !mov {
            m.made.push(t.to_path_buf());
            return;
        }
        if !in_place {
            if let Err(e) = remove_tree(s) {
                self.fail(s, format!("copied, but the original stayed: {e}"));
            }
        }
        m.moved.push((t.to_path_buf(), s.to_path_buf()));
    }

    /// Folder `s` into folder `t`: each item inside placed as at the top.
    /// A move removes `s` when it is left empty.
    fn merge(&mut self, s: &Path, t: &Path, mov: bool, m: &mut Moved) {
        let items: Vec<PathBuf> = match fs::read_dir(s) {
            Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
            Err(e) => return self.fail(s, e),
        };
        for c in items {
            if self.cancelled() {
                return;
            }
            if let Some(name) = c.file_name() {
                self.place(&c, &t.join(name), mov, m);
            }
        }
        if mov {
            let _ = fs::remove_dir(s);
        }
    }

    /// Back from the trash to `orig`, or beside it when the name was taken.
    fn restore_one(&mut self, file: &Path, info: &Path, orig: &Path) -> std::io::Result<PathBuf> {
        let target = if fs::symlink_metadata(orig).is_ok() {
            free_name(orig.parent().unwrap_or(Path::new("/")), orig)
        } else {
            orig.to_path_buf()
        };
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }
        self.move_one(file, &target)?;
        let _ = fs::remove_file(info);
        Ok(target)
    }

    /// Take `undo` back. Returns how to do it again (for redo).
    fn revert(&mut self, undo: Undo) -> Option<Undo> {
        match undo {
            Undo::MoveBack(pairs) => {
                // Each item goes back to its own folder (or name), never
                // over an item that took its place meanwhile.
                let pairs: Vec<(PathBuf, PathBuf)> = pairs
                    .into_iter()
                    .filter(|(now, _)| fs::symlink_metadata(now).is_ok())
                    .collect();
                for (_, orig) in &pairs {
                    if let Some(p) = orig.parent() {
                        let _ = fs::create_dir_all(p);
                    }
                }
                let dev = |p: &Path| fs::symlink_metadata(p).map(|m| m.dev()).ok();
                let (near, far): (Vec<_>, Vec<_>) = pairs
                    .into_iter()
                    .partition(|(now, orig)| dev(now) == orig.parent().and_then(dev));
                let mut done = match rename_all(&near) {
                    Ok(()) => near,
                    Err((done, msg)) => {
                        self.errors.push(msg);
                        done
                    }
                };
                // Another drive: copied back, as the move went.
                for (now, orig) in far {
                    if fs::symlink_metadata(&orig).is_ok() {
                        self.errors
                            .push(format!("\"{}\" already exists", orig.display()));
                    } else {
                        match self.move_one(&now, &orig) {
                            Ok(()) => done.push((now, orig)),
                            Err(e) => self.fail(&now, e),
                        }
                    }
                }
                let again = done.into_iter().map(|(now, orig)| (orig, now)).collect();
                Undo::steps(vec![Undo::MoveBack(again)])
            }
            Undo::Untrash(v) => {
                let mut back = Vec::new();
                for (file, info, orig) in &v {
                    match self.restore_one(file, info, orig) {
                        Ok(t) => back.push(t),
                        Err(e) => self.fail(file, e),
                    }
                }
                Undo::steps(vec![Undo::TrashCreated(back)])
            }
            Undo::TrashCreated(v) => {
                let mut gone = Vec::new();
                for p in v.iter().filter(|p| fs::symlink_metadata(p).is_ok()) {
                    match trash(p) {
                        Ok(t) => gone.push(t),
                        Err(e) => self.fail(p, e),
                    }
                }
                Undo::steps(vec![Undo::Untrash(gone)])
            }
            Undo::Steps(v) => {
                let mut again: Vec<Undo> = v.into_iter().filter_map(|u| self.revert(u)).collect();
                again.reverse();
                Undo::steps(again)
            }
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

/// The items an undo step leaves in place, to select after it.
fn touched(u: &Undo) -> Vec<PathBuf> {
    match u {
        Undo::MoveBack(v) => v.iter().map(|(now, _)| now.clone()).collect(),
        Undo::TrashCreated(v) => v.clone(),
        Undo::Untrash(_) => Vec::new(),
        Undo::Steps(v) => v.iter().flat_map(touched).collect(),
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
            tv_sec: sec as _,
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

/// Two paths that are one file: a change of case only on a drive that
/// ignores case (FAT, exFAT, Windows shares) finds the old file by the new name.
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// Rename in place without ever replacing another file. The kernel checks
/// and renames in one step (RENAME_NOREPLACE) where the file system can;
/// elsewhere (FUSE mounts, some network drives) a check just before has to do.
pub fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    if same_file(from, to) {
        return fs::rename(from, to);
    }
    let c = |p: &Path| {
        std::ffi::CString::new(p.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))
    };
    let (f, t) = (c(from)?, c(to)?);
    // The system call itself: the musl the release is linked with has no
    // renameat2() wrapper, the kernel has had the call since 3.15.
    let r = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            fs::rename(from, to)
        }
        _ => Err(e),
    }
}

/// Rename several items without replacing anything. When one takes a name
/// another gives up (a to b and b to c, or a swap), all first step aside to
/// hidden temporary names. A failure returns the renames that were made, so
/// they can still be undone, and the reason.
pub fn rename_all(moves: &[(PathBuf, PathBuf)]) -> Result<(), (Vec<(PathBuf, PathBuf)>, String)> {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let why = |e: &std::io::Error, f: &Path, t: &Path| rename_error(e, &name(f), &name(t));
    let sources: std::collections::HashSet<&PathBuf> = moves.iter().map(|m| &m.0).collect();
    let mut done = Vec::new();
    if !moves.iter().any(|(f, t)| f != t && sources.contains(t)) {
        for (f, t) in moves {
            if let Err(e) = rename_noreplace(f, t) {
                return Err((done, why(&e, f, t)));
            }
            done.push((f.clone(), t.clone()));
        }
        return Ok(());
    }
    let aside: Vec<PathBuf> = moves
        .iter()
        .enumerate()
        .map(|(k, (f, _))| f.with_file_name(format!(".nirufm-rename-{}-{k}", std::process::id())))
        .collect();
    for (k, (f, t)) in moves.iter().enumerate() {
        if let Err(e) = rename_noreplace(f, &aside[k]) {
            for j in 0..k {
                let _ = rename_noreplace(&aside[j], &moves[j].0);
            }
            return Err((done, why(&e, f, t)));
        }
    }
    let mut failed = None;
    for (k, (f, t)) in moves.iter().enumerate() {
        match rename_noreplace(&aside[k], t) {
            Ok(()) => done.push((f.clone(), t.clone())),
            Err(e) => {
                let mut msg = why(&e, f, t);
                if rename_noreplace(&aside[k], f).is_err() {
                    msg += &format!(" (kept as {})", name(&aside[k]));
                }
                failed.get_or_insert(msg);
            }
        }
    }
    failed.map_or(Ok(()), |m| Err((done, m)))
}

/// A failed rename in words: what went wrong and what to do about it.
pub fn rename_error(e: &std::io::Error, old: &str, new: &str) -> String {
    match e.raw_os_error() {
        Some(libc::EEXIST | libc::ENOTEMPTY) => format!("\"{new}\" already exists"),
        Some(libc::EACCES | libc::EPERM) => {
            format!("No permission to rename \"{old}\" in this folder")
        }
        Some(libc::EROFS) => format!("\"{old}\" is on a read-only drive"),
        Some(libc::ENOENT) => format!("\"{old}\" is no longer there"),
        Some(libc::ENAMETOOLONG) => "The name is too long for this drive".into(),
        Some(libc::EBUSY) => format!("\"{old}\" is in use (a mounted drive?)"),
        // FAT drives and Windows shares refuse some characters this way.
        Some(libc::EINVAL | libc::EILSEQ) => {
            "This drive does not allow that name (try without \\ : * ? \" < > |)".into()
        }
        _ if e.kind() == std::io::ErrorKind::AlreadyExists => {
            format!("\"{new}\" already exists")
        }
        _ => format!("Could not rename \"{old}\": {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One trash for all tests, never the user's: tests run side by side,
    /// so it is the same path for each of them (and for each run, so runs
    /// do not leave one behind each).
    fn private_trash() -> PathBuf {
        let data = std::env::temp_dir().join("nirufm-test-trash");
        std::env::set_var("XDG_DATA_HOME", &data);
        data
    }

    fn tmp(name: &str) -> PathBuf {
        private_trash();
        let d = std::env::temp_dir().join(format!("nirufm-ops-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(job: Job) -> (String, Option<Undo>, Vec<String>) {
        let (summary, undo, errors, asked) = answered(job, &[]);
        assert!(asked.is_empty(), "asked about {asked:?}");
        (summary, undo, errors)
    }

    /// Run a job, giving these answers to its questions in turn. Also
    /// returns the names it asked about.
    fn answered(
        job: Job,
        answers: &[(Choice, bool)],
    ) -> (String, Option<Undo>, Vec<String>, Vec<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK) };
        let w = start(tx, fd);
        w.tx.send((job, Arc::new(AtomicBool::new(false)))).unwrap();
        let mut asked = Vec::new();
        loop {
            match rx.recv().unwrap() {
                Msg::Ask { name, .. } => {
                    let (choice, all) = answers[asked.len()];
                    asked.push(name);
                    w.answer.send(Answer { choice, all }).unwrap();
                }
                Msg::Done {
                    summary,
                    undo,
                    errors,
                    ..
                } => {
                    unsafe { libc::close(fd) };
                    return (summary, undo, errors, asked);
                }
                Msg::Progress { .. } => {}
            }
        }
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap_or_else(|_| "<missing>".into())
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
        let (_, undo, errors, asked) = answered(
            Job::Copy {
                srcs: vec![d.join("src")],
                dst: d.join("dst"),
            },
            &[(Choice::KeepBoth, false)],
        );
        assert_eq!(asked, ["src"]);
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

    #[test]
    fn rename_never_replaces() {
        let d = tmp("rename");
        fs::write(d.join("a"), "a").unwrap();
        fs::write(d.join("b"), "b").unwrap();
        let e = rename_noreplace(&d.join("a"), &d.join("b")).unwrap_err();
        assert!(rename_error(&e, "a", "b").contains("already exists"));
        assert_eq!(fs::read_to_string(d.join("b")).unwrap(), "b");
        rename_noreplace(&d.join("a"), &d.join("c")).unwrap();
        assert!(!d.join("a").exists() && d.join("c").exists());
        let e = rename_noreplace(&d.join("gone"), &d.join("x")).unwrap_err();
        assert!(rename_error(&e, "gone", "x").contains("no longer there"));
        assert!(same_file(&d.join("c"), &d.join("c")));
        assert!(!same_file(&d.join("b"), &d.join("c")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rename_all_handles_chains_and_swaps() {
        let d = tmp("rename-all");
        for n in ["a", "b", "c"] {
            fs::write(d.join(n), n).unwrap();
        }
        let j = |n: &str| d.join(n);
        // A swap of a and b, and c to a new name.
        rename_all(&[(j("a"), j("b")), (j("b"), j("a")), (j("c"), j("d"))]).unwrap();
        assert_eq!(fs::read_to_string(j("a")).unwrap(), "b");
        assert_eq!(fs::read_to_string(j("b")).unwrap(), "a");
        assert_eq!(fs::read_to_string(j("d")).unwrap(), "c");
        // A name that is taken stops it, and what was done is returned.
        fs::write(j("e"), "e").unwrap();
        let (done, msg) = rename_all(&[(j("d"), j("f")), (j("a"), j("e"))]).unwrap_err();
        assert_eq!(done, vec![(j("d"), j("f"))]);
        assert!(msg.contains("already exists"));
        assert_eq!(fs::read_to_string(j("e")).unwrap(), "e");
        assert_eq!(fs::read_dir(&d).unwrap().count(), 4);
        let _ = fs::remove_dir_all(&d);
    }
    #[test]
    fn pasting_where_it_is_keeps_both_without_asking() {
        let d = tmp("same");
        fs::write(d.join("a.txt"), "a").unwrap();
        let (_, _, errors) = run(Job::Copy {
            srcs: vec![d.join("a.txt")],
            dst: d.clone(),
        });
        assert!(errors.is_empty());
        assert_eq!(read(&d.join("a (2).txt")), "a");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn replace_trashes_the_old_one_and_undo_and_redo_swap_them() {
        let d = tmp("replace");
        fs::create_dir_all(d.join("src")).unwrap();
        fs::create_dir_all(d.join("dst")).unwrap();
        fs::write(d.join("src/a.txt"), "new").unwrap();
        fs::write(d.join("dst/a.txt"), "old").unwrap();
        let (summary, undo, errors, asked) = answered(
            Job::Copy {
                srcs: vec![d.join("src/a.txt")],
                dst: d.join("dst"),
            },
            &[(Choice::Replace, false)],
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(asked, ["a.txt"]);
        assert!(summary.contains("1 replaced"), "{summary}");
        assert_eq!(read(&d.join("dst/a.txt")), "new");
        let names: Vec<_> = fs::read_dir(d.join("dst")).unwrap().flatten().collect();
        assert_eq!(names.len(), 1, "no temporary files are left");
        // Undo: the copy to the trash, the old one back.
        let (_, redo, errors) = run(Job::Revert {
            undo: undo.unwrap(),
        });
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(read(&d.join("dst/a.txt")), "old");
        // Redo: the other way round again.
        let (_, again, errors) = run(Job::Revert {
            undo: redo.unwrap(),
        });
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(read(&d.join("dst/a.txt")), "new");
        run(Job::Revert {
            undo: again.unwrap(),
        });
        assert_eq!(read(&d.join("dst/a.txt")), "old");
        assert_eq!(read(&d.join("src/a.txt")), "new");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_copy_never_costs_the_existing_file() {
        let d = tmp("replace-fail");
        fs::create_dir_all(d.join("src")).unwrap();
        fs::create_dir_all(d.join("dst")).unwrap();
        fs::write(d.join("src/a.txt"), "new").unwrap();
        fs::write(d.join("dst/a.txt"), "old").unwrap();
        fs::set_permissions(d.join("src/a.txt"), fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(d.join("src/a.txt")).is_ok() {
            // Run as root: the file stays readable, nothing to test.
            let _ = fs::remove_dir_all(&d);
            return;
        }
        let (_, _, errors, _) = answered(
            Job::Copy {
                srcs: vec![d.join("src/a.txt")],
                dst: d.join("dst"),
            },
            &[(Choice::Replace, false)],
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(read(&d.join("dst/a.txt")), "old");
        assert_eq!(fs::read_dir(d.join("dst")).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn skip_and_keep_both_apply_to_all() {
        let d = tmp("all");
        fs::create_dir_all(d.join("src")).unwrap();
        fs::create_dir_all(d.join("dst")).unwrap();
        for n in ["a", "b", "c"] {
            fs::write(d.join("src").join(n), "new").unwrap();
            fs::write(d.join("dst").join(n), "old").unwrap();
        }
        let srcs: Vec<PathBuf> = ["a", "b", "c"]
            .iter()
            .map(|n| d.join("src").join(n))
            .collect();
        let (summary, _, _, asked) = answered(
            Job::Copy {
                srcs: srcs.clone(),
                dst: d.join("dst"),
            },
            &[(Choice::Skip, false), (Choice::KeepBoth, true)],
        );
        assert_eq!(asked.len(), 2, "the second answer was for all");
        assert!(summary.contains("1 skipped"), "{summary}");
        assert!(!d.join("dst/a (2)").exists());
        assert_eq!(read(&d.join("dst/b (2)")), "new");
        assert_eq!(read(&d.join("dst/c (2)")), "new");
        assert_eq!(read(&d.join("dst/a")), "old");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn merging_folders_asks_inside_and_undoes() {
        let d = tmp("merge");
        fs::create_dir_all(d.join("src/f/deep")).unwrap();
        fs::create_dir_all(d.join("dst/f")).unwrap();
        fs::write(d.join("src/f/x"), "x").unwrap();
        fs::write(d.join("src/f/y"), "new y").unwrap();
        fs::write(d.join("src/f/deep/z"), "z").unwrap();
        fs::write(d.join("dst/f/y"), "old y").unwrap();
        fs::write(d.join("dst/f/w"), "w").unwrap();
        let (_, undo, errors, asked) = answered(
            Job::Move {
                srcs: vec![d.join("src/f")],
                dst: d.join("dst"),
            },
            &[(Choice::Merge, false), (Choice::Replace, false)],
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(asked, ["f", "y"]);
        assert_eq!(read(&d.join("dst/f/x")), "x");
        assert_eq!(read(&d.join("dst/f/y")), "new y");
        assert_eq!(read(&d.join("dst/f/w")), "w");
        assert_eq!(read(&d.join("dst/f/deep/z")), "z");
        assert!(!d.join("src/f").exists(), "the emptied folder goes");
        let (_, _, errors) = run(Job::Revert {
            undo: undo.unwrap(),
        });
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(read(&d.join("src/f/x")), "x");
        assert_eq!(read(&d.join("src/f/y")), "new y");
        assert_eq!(read(&d.join("src/f/deep/z")), "z");
        assert_eq!(read(&d.join("dst/f/y")), "old y");
        assert_eq!(read(&d.join("dst/f/w")), "w");
        assert!(!d.join("dst/f/x").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn replacing_a_folder_with_a_file_inside_it_is_refused() {
        let d = tmp("inside");
        fs::create_dir_all(d.join("p/d")).unwrap();
        fs::write(d.join("p/d/d"), "file").unwrap();
        let (_, _, errors, asked) = answered(
            Job::Move {
                srcs: vec![d.join("p/d/d")],
                dst: d.join("p"),
            },
            &[(Choice::Replace, false)],
        );
        assert_eq!(asked, ["d"]);
        assert_eq!(errors.len(), 1);
        assert_eq!(read(&d.join("p/d/d")), "file");
        let _ = fs::remove_dir_all(&d);
    }
    #[test]
    fn trash_and_move_undo_and_redo_back_and_forth() {
        let d = tmp("redo");
        fs::create_dir_all(d.join("to")).unwrap();
        fs::write(d.join("a"), "a").unwrap();
        fs::write(d.join("b"), "b").unwrap();
        let (_, undo, _) = run(Job::Trash {
            paths: vec![d.join("a")],
        });
        assert!(!d.join("a").exists());
        let (_, redo, _) = run(Job::Revert {
            undo: undo.unwrap(),
        });
        assert_eq!(read(&d.join("a")), "a");
        let (_, undo, _) = run(Job::Revert {
            undo: redo.unwrap(),
        });
        assert!(!d.join("a").exists());
        run(Job::Revert {
            undo: undo.unwrap(),
        });
        assert_eq!(read(&d.join("a")), "a");
        let (_, undo, _) = run(Job::Move {
            srcs: vec![d.join("b")],
            dst: d.join("to"),
        });
        let (_, redo, _) = run(Job::Revert {
            undo: undo.unwrap(),
        });
        assert_eq!(read(&d.join("b")), "b");
        run(Job::Revert {
            undo: redo.unwrap(),
        });
        assert_eq!(read(&d.join("to/b")), "b");
        assert!(!d.join("b").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
