//! The file manager: tabs, the side panel, the path bar, the icon and list
//! views, and all keyboard and mouse handling. Drawing happens only when
//! something changed; between events the process sleeps in `poll`.
use crate::canvas::{Canvas, Rect};
use crate::dir::{self, Entry, SortKey};
use crate::icons::Icons;
use crate::mime::Mime;
use crate::places::{self, Place};
use crate::text::{Size, Text};
use crate::theme::{self, blend, Palette};
use crate::x::{Res, X};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Icons,
    List,
}

struct Tab {
    path: PathBuf,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    all: Vec<Entry>,
    /// Indices into `all` that are shown (hidden files filtered).
    shown: Vec<usize>,
    selected: Vec<bool>,
    cursor: usize,
    anchor: usize,
    scroll: i32,
    error: Option<String>,
}

impl Tab {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            back: Vec::new(),
            forward: Vec::new(),
            all: Vec::new(),
            shown: Vec::new(),
            selected: Vec::new(),
            cursor: 0,
            anchor: 0,
            scroll: 0,
            error: None,
        }
    }
    fn entry(&self, i: usize) -> Option<&Entry> {
        self.shown.get(i).and_then(|j| self.all.get(*j))
    }
    fn title(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| places::pretty(&self.path))
    }
}

#[derive(Clone)]
enum Hit {
    Place(usize),
    Tab(usize),
    TabClose(usize),
    Crumb(PathBuf),
    Back,
    Forward,
    Up,
    Item(usize),
    Header(SortKey),
    Content,
    MenuItem(usize),
    Eject(usize),
    MenuOutside,
}

struct Metrics {
    s: f32,
    font: Size,
    small: Size,
    pad: i32,
    bar_h: i32,
    tab_h: i32,
    status_h: i32,
    side_w: i32,
    icon: i32,
    small_icon: i32,
    cell_w: i32,
    cell_h: i32,
    row_h: i32,
}

pub struct App {
    x: X,
    cv: Canvas,
    text: Text,
    icons: Icons,
    mime: Mime,
    p: Palette,
    theme: String,
    m: Metrics,
    tabs: Vec<Tab>,
    active: usize,
    view: View,
    hidden: bool,
    sort: SortKey,
    reverse: bool,
    places: Vec<Place>,
    hits: Vec<(Rect, Hit)>,
    content: Rect,
    cols: i32,
    hover: Option<usize>,
    last_click: Option<(Instant, usize)>,
    typed: String,
    typed_at: Instant,
    path_edit: Option<String>,
    dirty: bool,
    quit: bool,
    inotify: i32,
    watch: i32,
    reload_at: Option<Instant>,
    started: Instant,
    shown_once: bool,
    message: Option<(String, Instant)>,
    error: bool,
    worker: crate::ops::Worker,
    rx: std::sync::mpsc::Receiver<crate::ops::Msg>,
    wake: i32,
    running: Option<Running>,
    pending: usize,
    undo: Vec<crate::ops::Undo>,
    clip: Option<(Vec<PathBuf>, bool)>,
    menu: Option<Menu>,
    confirm: Option<Confirm>,
    rename: Option<(usize, String)>,
    select_after: Vec<PathBuf>,
    zoom: usize,
    font_px: f32,
    thumbs: crate::thumbs::Thumbs,
    thumb_types: std::collections::HashMap<String, ()>,
    thumb_img: std::collections::HashMap<PathBuf, Option<std::rc::Rc<crate::canvas::Image>>>,
    preview: Option<Preview>,
    preview_tx: std::sync::mpsc::Sender<Preview>,
    preview_rx: std::sync::mpsc::Receiver<Preview>,
    dev_watch: i32,
    mounts_fd: i32,
    connect: Option<ConnectForm>,
    menu_place: Option<usize>,
    side_scroll: i32,
    side_total: i32,
}

struct ConnectForm {
    uri: String,
    user: String,
    pass: String,
    /// 0 address, 1 user, 2 password.
    field: usize,
    login: bool,
    note: Option<String>,
}

struct Preview {
    path: PathBuf,
    image: Option<crate::canvas::Image>,
    lines: Vec<String>,
    note: Option<String>,
    ready: bool,
}

struct Running {
    verb: &'static str,
    done: u64,
    total: u64,
    current: String,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Open,
    OpenTab,
    Cut,
    Copy,
    Paste,
    Rename,
    Trash,
    Delete,
    Restore,
    EmptyTrash,
    Extract,
    PackZip,
    PackTar,
    NewFolder,
    Terminal,
    Hidden,
    CopyPath,
    Bookmark,
    RemoveBookmark,
    Connect,
    NcPublic,
    NcInternal,
    NcShare,
    NcBrowser,
}

struct Menu {
    x: i32,
    y: i32,
    items: Vec<(String, Act, &'static str)>,
    hover: Option<usize>,
}

struct Confirm {
    text: String,
    job: crate::ops::Job,
}

static NET_WATCH: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-2);

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

impl App {
    pub fn new(start: PathBuf, started: Instant) -> Res<Self> {
        let base = theme::config_home();
        let font_px = theme::configured_font_size(&base).unwrap_or(15.0);
        let text = Text::new(theme::configured_font_path(&base).as_deref())?;
        let s = (font_px / 15.0).clamp(0.6, 3.0);
        let m = metrics(&text, font_px, s, ZOOM_DEFAULT);
        let x = X::open((1120.0 * s) as u16, (720.0 * s) as u16, "NIRUFM")?;
        let theme_name = x
            .root_theme()
            .unwrap_or_else(|| theme::configured_theme(&base));
        let p = theme::load(&base, &theme_name);
        let _ = x.set_icon(&crate::logo::window_icons(&p));
        let inotify = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        let wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = crate::ops::start(tx, wake);
        let (thumbs, thumb_types) = crate::thumbs::Thumbs::start(wake);
        let (preview_tx, preview_rx) = std::sync::mpsc::channel();
        // A drive plugged in or pulled out shows up in /dev/disk/by-id;
        // a mount or unmount makes /proc/self/mounts readable with POLLPRI.
        let dev_watch = if inotify >= 0 {
            unsafe {
                libc::inotify_add_watch(
                    inotify,
                    c"/dev/disk/by-id".as_ptr(),
                    libc::IN_CREATE | libc::IN_DELETE,
                )
            }
        } else {
            -1
        };
        if inotify >= 0 {
            use std::os::unix::ffi::OsStrExt;
            if let Ok(c) = std::ffi::CString::new(crate::remote::gvfs_root().as_os_str().as_bytes())
            {
                // Server mounts appear here; refresh the panel like for drives.
                let wd = unsafe {
                    libc::inotify_add_watch(inotify, c.as_ptr(), libc::IN_CREATE | libc::IN_DELETE)
                };
                NET_WATCH.store(wd, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let mounts_fd = unsafe {
            libc::open(
                c"/proc/self/mounts".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        let mut app = App {
            cv: Canvas::new((1120.0 * s) as i32, (720.0 * s) as i32),
            icons: Icons::new(p.is_light()),
            mime: Mime::load(),
            x,
            text,
            p,
            theme: theme_name,
            m,
            tabs: vec![Tab::new(start)],
            active: 0,
            view: View::Icons,
            hidden: false,
            sort: SortKey::Name,
            reverse: false,
            places: places::places(),
            hits: Vec::new(),
            content: Rect::default(),
            cols: 1,
            hover: None,
            last_click: None,
            typed: String::new(),
            typed_at: Instant::now(),
            path_edit: None,
            dirty: true,
            quit: false,
            inotify,
            watch: -1,
            reload_at: None,
            started,
            shown_once: false,
            message: None,
            error: false,
            worker,
            rx,
            wake,
            running: None,
            pending: 0,
            undo: Vec::new(),
            clip: None,
            menu: None,
            confirm: None,
            rename: None,
            select_after: Vec::new(),
            zoom: ZOOM_DEFAULT,
            font_px,
            thumbs,
            thumb_types,
            thumb_img: std::collections::HashMap::new(),
            preview: None,
            preview_tx,
            preview_rx,
            dev_watch,
            mounts_fd,
            connect: None,
            menu_place: None,
            side_scroll: 0,
            side_total: 0,
        };
        app.load_tab(0, true);
        app.x.map()?;
        Ok(app)
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }
    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    // ---------------------------------------------------------------- data

    fn load_tab(&mut self, i: usize, reset: bool) {
        let keep = (!reset)
            .then(|| {
                self.tabs[i]
                    .entry(self.tabs[i].cursor)
                    .map(|e| e.name.clone())
            })
            .flatten();
        let keep_sel: Vec<String> = if reset {
            Vec::new()
        } else {
            let t = &self.tabs[i];
            t.selected
                .iter()
                .enumerate()
                .filter(|(_, s)| **s)
                .filter_map(|(k, _)| t.entry(k).map(|e| e.name.clone()))
                .collect()
        };
        let path = self.tabs[i].path.clone();
        if reset {
            self.thumbs.clear();
            self.thumb_img.clear();
        }
        let (all, error) = match dir::read(&path, &self.mime) {
            Ok(mut v) => {
                dir::sort(&mut v, self.sort, self.reverse);
                (v, None)
            }
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        let hidden = self.hidden;
        let t = &mut self.tabs[i];
        t.all = all;
        t.error = error;
        t.shown = (0..t.all.len())
            .filter(|k| hidden || !t.all[*k].hidden())
            .collect();
        t.selected = t
            .shown
            .iter()
            .map(|k| keep_sel.contains(&t.all[*k].name))
            .collect();
        t.cursor = keep
            .and_then(|n| t.shown.iter().position(|k| t.all[*k].name == n))
            .unwrap_or(0);
        t.anchor = t.cursor;
        if reset {
            t.scroll = 0;
        }
        if i == self.active {
            self.rewatch();
            let _ = self
                .x
                .set_title(&format!("{} · NIRUFM", places::pretty(&self.tabs[i].path)));
        }
        self.dirty = true;
    }

    fn rewatch(&mut self) {
        if self.inotify < 0 {
            return;
        }
        if self.watch >= 0 {
            unsafe { libc::inotify_rm_watch(self.inotify, self.watch) };
            self.watch = -1;
        }
        use std::os::unix::ffi::OsStrExt;
        if let Ok(c) = std::ffi::CString::new(self.tab().path.as_os_str().as_bytes()) {
            let mask = libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_CLOSE_WRITE
                | libc::IN_ATTRIB
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF;
            self.watch = unsafe { libc::inotify_add_watch(self.inotify, c.as_ptr(), mask) };
        }
    }

    fn drain_inotify(&mut self) {
        let mut buf = [0u8; 4096];
        let (mut folder, mut devices) = (false, false);
        loop {
            let n = unsafe {
                libc::read(
                    self.inotify,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
            let mut off = 0usize;
            let hdr = std::mem::size_of::<libc::inotify_event>();
            while off + hdr <= n as usize {
                let ev: libc::inotify_event =
                    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const _) };
                if ev.wd == self.dev_watch
                    || ev.wd == NET_WATCH.load(std::sync::atomic::Ordering::Relaxed)
                {
                    devices = true;
                } else {
                    folder = true;
                }
                off += hdr + ev.len as usize;
            }
        }
        if folder {
            // Many events arrive for one copy: reload once, a moment later.
            self.reload_at
                .get_or_insert(Instant::now() + Duration::from_millis(120));
        }
        if devices {
            self.refresh_places();
        }
    }

    fn refresh_places(&mut self) {
        let p = places::places();
        if p != self.places {
            self.places = p;
            self.dirty = true;
        }
    }

    fn go(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if path == self.tab().path {
            return;
        }
        let old = self.tab().path.clone();
        let t = self.tab_mut();
        t.back.push(old.clone());
        t.forward.clear();
        t.path = path;
        let a = self.active;
        self.load_tab(a, true);
        // Coming up from a child folder selects that folder.
        let t = self.tab_mut();
        if let Some(pos) = t.shown.iter().position(|k| t.all[*k].path == old) {
            t.cursor = pos;
            t.anchor = pos;
        }
        self.path_edit = None;
    }

    fn back(&mut self) {
        let Some(p) = self.tab_mut().back.pop() else {
            return;
        };
        let cur = self.tab().path.clone();
        self.tab_mut().forward.push(cur);
        self.tab_mut().path = p;
        let a = self.active;
        self.load_tab(a, true);
    }

    fn forward(&mut self) {
        let Some(p) = self.tab_mut().forward.pop() else {
            return;
        };
        let cur = self.tab().path.clone();
        self.tab_mut().back.push(cur);
        self.tab_mut().path = p;
        let a = self.active;
        self.load_tab(a, true);
    }

    fn up(&mut self) {
        if let Some(parent) = self.tab().path.parent().map(Path::to_path_buf) {
            self.go(parent);
        }
    }

    fn new_tab(&mut self, path: PathBuf) {
        self.tabs.insert(self.active + 1, Tab::new(path));
        self.active += 1;
        let a = self.active;
        self.load_tab(a, true);
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs.len() == 1 {
            self.quit = true;
            return;
        }
        self.tabs.remove(i);
        if self.active >= self.tabs.len() || self.active > i {
            self.active = self.active.saturating_sub(1);
        }
        let a = self.active;
        self.load_tab(a, false);
    }

    fn switch(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.active = i;
            let a = self.active;
            self.load_tab(a, false);
        }
    }

    fn open(&mut self, i: usize) {
        let Some(e) = self.tab().entry(i).cloned() else {
            return;
        };
        if e.is_dir {
            self.go(e.path);
        } else {
            spawn("xdg-open", &[e.path.as_os_str()], None);
            self.flash(format!("Opening {}", e.name));
        }
    }

    fn open_selection(&mut self) {
        let t = self.tab();
        let picked: Vec<usize> = t
            .selected
            .iter()
            .enumerate()
            .filter(|(_, s)| **s)
            .map(|(i, _)| i)
            .collect();
        if picked.len() <= 1 {
            let c = t.cursor;
            self.open(c);
            return;
        }
        // Several files open each; a folder among them is opened in a tab.
        for i in picked {
            if let Some(e) = self.tab().entry(i).cloned() {
                if e.is_dir {
                    self.new_tab(e.path);
                } else {
                    spawn("xdg-open", &[e.path.as_os_str()], None);
                }
            }
        }
    }

    fn flash(&mut self, s: String) {
        self.message = Some((s, Instant::now()));
        self.dirty = true;
    }

    fn reload_theme(&mut self) {
        let base = theme::config_home();
        let name = self
            .x
            .root_theme()
            .unwrap_or_else(|| theme::configured_theme(&base));
        let p = theme::load(&base, &name);
        if p != self.p || name != self.theme {
            self.p = p;
            self.theme = name;
            self.icons.set_light(p.is_light());
            let _ = self.x.set_icon(&crate::logo::window_icons(&p));
            self.dirty = true;
        }
    }

    // ------------------------------------------------------------ selection

    fn set_cursor(&mut self, i: usize, extend: bool, toggle: bool) {
        let t = self.tab_mut();
        if t.shown.is_empty() {
            return;
        }
        let i = i.min(t.shown.len() - 1);
        t.cursor = i;
        if toggle {
            t.selected[i] = !t.selected[i];
            t.anchor = i;
        } else if extend {
            let (a, b) = (t.anchor.min(i), t.anchor.max(i));
            for (k, s) in t.selected.iter_mut().enumerate() {
                *s = k >= a && k <= b;
            }
        } else {
            t.selected.iter_mut().for_each(|s| *s = false);
            t.selected[i] = true;
            t.anchor = i;
        }
        self.reveal();
        self.dirty = true;
    }

    fn select_all(&mut self) {
        self.tab_mut().selected.iter_mut().for_each(|s| *s = true);
        self.dirty = true;
    }

    fn clear_selection(&mut self) {
        self.tab_mut().selected.iter_mut().for_each(|s| *s = false);
        self.dirty = true;
    }

    fn item_rect(&self, i: usize) -> Rect {
        let c = self.content;
        match self.view {
            View::Icons => {
                let cols = self.cols.max(1);
                let (col, row) = (i as i32 % cols, i as i32 / cols);
                Rect::new(
                    c.x + self.m.pad + col * self.m.cell_w,
                    c.y + self.m.pad + row * self.m.cell_h - self.tab().scroll,
                    self.m.cell_w,
                    self.m.cell_h,
                )
            }
            View::List => Rect::new(
                c.x,
                c.y + self.m.row_h + i as i32 * self.m.row_h - self.tab().scroll,
                c.w,
                self.m.row_h,
            ),
        }
    }

    fn content_height(&self) -> i32 {
        let n = self.tab().shown.len() as i32;
        match self.view {
            View::Icons => {
                let rows = (n + self.cols.max(1) - 1) / self.cols.max(1);
                rows * self.m.cell_h + 2 * self.m.pad
            }
            View::List => (n + 1) * self.m.row_h,
        }
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_height() - self.content.h).max(0);
        let t = self.tab_mut();
        t.scroll = t.scroll.clamp(0, max);
    }

    fn reveal(&mut self) {
        let i = self.tab().cursor;
        let r = self.item_rect(i);
        let top = self.content.y
            + if self.view == View::List {
                self.m.row_h
            } else {
                0
            };
        let bottom = self.content.y + self.content.h;
        let t = self.tab_mut();
        if r.y < top {
            t.scroll -= top - r.y;
        } else if r.y + r.h > bottom {
            t.scroll += r.y + r.h - bottom;
        }
        self.clamp_scroll();
    }

    fn page(&self) -> usize {
        match self.view {
            View::Icons => ((self.content.h / self.m.cell_h).max(1) * self.cols.max(1)) as usize,
            View::List => (self.content.h / self.m.row_h - 1).max(1) as usize,
        }
    }

    // -------------------------------------------------------------- events

    pub fn run(&mut self) -> Res<()> {
        let xfd = {
            use std::os::fd::AsRawFd;
            self.x.c.stream().as_raw_fd()
        };
        while !self.quit {
            while let Some(ev) = self.x.c.poll_for_event()? {
                self.event(ev)?;
            }
            if self.quit {
                break;
            }
            if let Some(at) = self.reload_at {
                if Instant::now() >= at {
                    self.reload_at = None;
                    let a = self.active;
                    self.load_tab(a, false);
                }
            }
            if self
                .message
                .as_ref()
                .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(3))
            {
                self.message = None;
                self.dirty = true;
            }
            if self.dirty {
                self.draw();
                self.x.present(&self.cv)?;
                self.dirty = false;
                if !self.shown_once {
                    self.shown_once = true;
                    if std::env::var_os("NIRUFM_TIMING").is_some() {
                        eprintln!(
                            "nirufm: first frame after {:.1} ms",
                            self.started.elapsed().as_secs_f64() * 1000.0
                        );
                    }
                }
                continue;
            }
            self.x.c.flush()?;
            let mut timeout: i32 = -1;
            if let Some(at) = self.reload_at {
                timeout = at.saturating_duration_since(Instant::now()).as_millis() as i32 + 1;
            }
            if let Some((_, t)) = &self.message {
                let left = Duration::from_secs(3)
                    .saturating_sub(t.elapsed())
                    .as_millis() as i32
                    + 1;
                timeout = if timeout < 0 { left } else { timeout.min(left) };
            }
            let fds = [
                libc::pollfd {
                    fd: xfd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.wake,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.inotify,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let mut fds4 = [
                fds[0],
                fds[1],
                fds[2],
                libc::pollfd {
                    fd: self.mounts_fd,
                    events: libc::POLLPRI,
                    revents: 0,
                },
            ];
            let n = if self.inotify >= 0 { 3 } else { 2 };
            let total = if self.mounts_fd >= 0 { 4 } else { n };
            if self.inotify < 0 {
                fds4[2] = fds4[3];
            }
            unsafe { libc::poll(fds4.as_mut_ptr(), total as libc::nfds_t, timeout) };
            let fds = [fds4[0], fds4[1], fds4[2]];
            if self.mounts_fd >= 0 && fds4[3].revents & (libc::POLLPRI | libc::POLLERR) != 0 {
                self.refresh_places();
            }
            if fds[1].revents & libc::POLLIN != 0 {
                let mut v = 0u64;
                unsafe { libc::read(self.wake, &mut v as *mut u64 as *mut libc::c_void, 8) };
                self.worker_messages();
            }
            if n == 3 && fds[2].revents & libc::POLLIN != 0 {
                self.drain_inotify();
            }
        }
        Ok(())
    }

    fn event(&mut self, ev: Event) -> Res<()> {
        match ev {
            Event::Expose(e) if e.count == 0 => self.dirty = true,
            Event::ConfigureNotify(e) => {
                if e.width as i32 != self.cv.w || e.height as i32 != self.cv.h {
                    self.cv.resize(e.width as i32, e.height as i32);
                    self.dirty = true;
                }
            }
            Event::ClientMessage(e) => {
                if e.type_ == self.x.atoms.wm_protocols
                    && e.data.as_data32()[0] == self.x.atoms.wm_delete
                {
                    self.quit = true;
                }
            }
            Event::PropertyNotify(e) => {
                if e.window == self.x.root && e.atom == self.x.atoms.niruwm_theme {
                    self.reload_theme();
                }
            }
            Event::SelectionRequest(e) => {
                if let Some((files, cut)) = self.clip.clone() {
                    let _ = self.x.answer(&e, &files, cut);
                }
            }
            Event::SelectionClear(_) => {}
            Event::SelectionNotify(e) => self.pasted(e.target, e.property),
            Event::MotionNotify(e) if self.menu.is_some() => {
                let (x, y) = (e.event_x as i32, e.event_y as i32);
                let h = match self.hit(x, y) {
                    Some(Hit::MenuItem(i)) => Some(i),
                    _ => None,
                };
                if let Some(m) = self.menu.as_mut() {
                    if m.hover != h {
                        m.hover = h;
                        self.dirty = true;
                    }
                }
            }
            Event::MotionNotify(e) => {
                let h = match self.hit(e.event_x as i32, e.event_y as i32) {
                    Some(Hit::Item(i)) => Some(i),
                    _ => None,
                };
                if h != self.hover {
                    self.hover = h;
                    self.dirty = true;
                }
            }
            Event::LeaveNotify(_) => {
                if self.hover.take().is_some() {
                    self.dirty = true;
                }
            }
            Event::ButtonPress(e) => {
                self.button(e.detail, e.event_x as i32, e.event_y as i32, e.state)
            }
            Event::KeyPress(e) => self.key(e.detail, e.state),
            _ => {}
        }
        Ok(())
    }

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| h.clone())
    }

    fn button(&mut self, b: u8, x: i32, y: i32, state: KeyButMask) {
        let ctrl = state.contains(KeyButMask::CONTROL);
        let shift = state.contains(KeyButMask::SHIFT);
        if self.confirm.is_some() {
            return;
        }
        if self.preview.is_some() {
            self.preview = None;
            self.dirty = true;
            return;
        }
        if self.menu.is_some() {
            if let (1 | 3, Some(Hit::MenuItem(i))) = (b, self.hit(x, y)) {
                let act = self
                    .menu
                    .as_ref()
                    .and_then(|m| m.items.get(i))
                    .map(|it| it.1);
                self.menu = None;
                if let Some(a) = act {
                    self.act(a);
                }
            } else {
                self.menu = None;
            }
            self.dirty = true;
            return;
        }
        if self.rename.is_some() {
            self.commit_rename();
        }
        if b == 3 {
            match self.hit(x, y) {
                Some(Hit::Place(i)) if self.places.get(i).is_some_and(|p| p.bookmark) => {
                    self.menu_place = Some(i);
                    self.menu = Some(Menu {
                        x,
                        y,
                        items: vec![("Remove bookmark".into(), Act::RemoveBookmark, "")],
                        hover: None,
                    });
                    self.dirty = true;
                }
                Some(Hit::Item(i)) => {
                    if !self.tab().selected.get(i).copied().unwrap_or(false) {
                        self.set_cursor(i, false, false);
                    }
                    self.open_menu(x, y, true);
                }
                Some(Hit::Content) => {
                    self.clear_selection();
                    self.open_menu(x, y, false);
                }
                _ => {}
            }
            return;
        }
        let step = self.m.row_h * 3;
        match b {
            4 | 5 if ctrl => {
                if b == 4 {
                    self.set_zoom(self.zoom + 1);
                } else {
                    self.set_zoom(self.zoom.saturating_sub(1));
                }
                return;
            }
            4 | 5 if x < self.m.side_w => {
                let max = (self.side_total - self.cv.h).max(0);
                let d = if b == 4 { -step } else { step };
                self.side_scroll = (self.side_scroll + d).clamp(0, max);
                self.dirty = true;
                return;
            }
            4 | 5 => {
                if self.content.contains(x, y) {
                    let d = if b == 4 { -step } else { step };
                    self.tab_mut().scroll += d;
                    self.clamp_scroll();
                    self.dirty = true;
                }
                return;
            }
            8 => return self.back(),
            9 => return self.forward(),
            _ => {}
        }
        let Some(h) = self.hit(x, y) else { return };
        match (b, h) {
            (1, Hit::Place(i)) => {
                if let Some(pl) = self.places.get(i).cloned() {
                    if pl.connect {
                        self.open_connect(None, None);
                    } else if let (Some(dev), false) = (&pl.dev, pl.mounted) {
                        self.submit(crate::ops::Job::Mount { dev: dev.clone() });
                    } else if let (Some(uri), false) = (&pl.uri, pl.mounted) {
                        self.submit(crate::ops::Job::Connect {
                            uri: uri.clone(),
                            login: None,
                        });
                    } else {
                        self.go(pl.path);
                    }
                }
            }
            (1, Hit::Eject(i)) => {
                if let Some(pl) = self.places.get(i).cloned() {
                    if let (Some(uri), None) = (&pl.uri, &pl.dev) {
                        for t in self
                            .tabs
                            .iter_mut()
                            .filter(|t| t.path.starts_with(&pl.path))
                        {
                            t.path = theme::home();
                        }
                        let a = self.active;
                        self.load_tab(a, true);
                        self.submit(crate::ops::Job::Disconnect { uri: uri.clone() });
                        return;
                    }
                    if let Some(dev) = pl.dev {
                        // Leave the drive first, or it stays busy.
                        if self.tabs.iter().any(|t| t.path.starts_with(&pl.path)) {
                            for t in self
                                .tabs
                                .iter_mut()
                                .filter(|t| t.path.starts_with(&pl.path))
                            {
                                t.path = theme::home();
                            }
                            let a = self.active;
                            self.load_tab(a, true);
                        }
                        self.submit(crate::ops::Job::Eject { dev });
                    }
                }
            }
            (2, Hit::Place(i)) => {
                if let Some(p) = self.places.get(i).map(|p| p.path.clone()) {
                    self.new_tab(p);
                }
            }
            (1, Hit::Tab(i)) => self.switch(i),
            (2, Hit::Tab(i)) | (1, Hit::TabClose(i)) => self.close_tab(i),
            (1, Hit::Crumb(p)) => self.go(p),
            (2, Hit::Crumb(p)) => self.new_tab(p),
            (1, Hit::Back) => self.back(),
            (1, Hit::Forward) => self.forward(),
            (1, Hit::Up) => self.up(),
            (1, Hit::Header(k)) => {
                if self.sort == k {
                    self.reverse = !self.reverse;
                } else {
                    self.sort = k;
                    self.reverse = false;
                }
                let a = self.active;
                self.load_tab(a, false);
            }
            (1, Hit::Item(i)) => {
                let double = self
                    .last_click
                    .is_some_and(|(t, j)| j == i && t.elapsed() < DOUBLE_CLICK);
                if double && !ctrl && !shift {
                    self.last_click = None;
                    self.open(i);
                } else {
                    self.last_click = Some((Instant::now(), i));
                    self.set_cursor(i, shift, ctrl);
                }
            }
            (2, Hit::Item(i)) => {
                if let Some(e) = self.tab().entry(i).cloned() {
                    if e.is_dir {
                        self.new_tab(e.path);
                    }
                }
            }
            (1, Hit::Content) if !ctrl && !shift => self.clear_selection(),
            _ => {}
        }
    }

    fn key(&mut self, code: u8, state: KeyButMask) {
        let sym = self.x.keysym(code, state);
        let ctrl = state.contains(KeyButMask::CONTROL);
        let shift = state.contains(KeyButMask::SHIFT);
        let alt = state.contains(KeyButMask::MOD1);
        if self.connect.is_some() {
            return self.connect_key(sym, ctrl, shift);
        }
        if self.confirm.is_some() {
            match sym {
                0xff0d | 0xff8d | 0x79 | 0x6a => {
                    if let Some(c) = self.confirm.take() {
                        self.submit(c.job);
                    }
                }
                0xff1b | 0x6e => self.confirm = None,
                _ => {}
            }
            self.dirty = true;
            return;
        }
        if let Some(m) = self.menu.as_mut() {
            let n = m.items.len();
            match sym {
                0xff1b => self.menu = None,
                0xff52 => m.hover = Some(m.hover.map_or(n - 1, |h| (h + n - 1) % n)),
                0xff54 => m.hover = Some(m.hover.map_or(0, |h| (h + 1) % n)),
                0xff0d | 0xff8d => {
                    let act = m.hover.and_then(|h| m.items.get(h)).map(|it| it.1);
                    self.menu = None;
                    if let Some(a) = act {
                        self.act(a);
                    }
                }
                _ => {}
            }
            self.dirty = true;
            return;
        }
        if self.rename.is_some() {
            return self.rename_key(sym, ctrl);
        }
        if self.preview.is_some() {
            return self.preview_key(sym);
        }
        if self.path_edit.is_some() {
            return self.path_key(sym, ctrl);
        }
        let t = self.tab();
        let (cur, n) = (t.cursor, t.shown.len());
        let cols = if self.view == View::Icons {
            self.cols.max(1) as usize
        } else {
            1
        };
        // keysyms: X11/keysymdef.h
        match sym {
            0xff1b => {
                // Escape: cancel a running job, else clear the selection.
                if let Some(r) = &self.running {
                    r.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.flash("Cancelling…".into());
                } else {
                    self.typed.clear();
                    self.clear_selection();
                }
            }
            0xffff => {
                // Delete; Shift+Delete deletes permanently after asking.
                if shift || self.in_trash() {
                    self.act(Act::Delete);
                } else {
                    self.act(Act::Trash);
                }
            }
            0xffbf => self.act(Act::Rename), // F2
            0x20 if !ctrl
                && (self.typed.is_empty()
                    || self.typed_at.elapsed() > Duration::from_millis(900)) =>
            {
                self.toggle_preview()
            }
            0xff67 => {
                // Menu key: the context menu at the cursor.
                let c = self.tab().cursor;
                let r = self.item_rect(c);
                let any = !self.tab().shown.is_empty();
                self.open_menu(r.x + r.w / 2, r.y + r.h / 2, any);
            }
            0xff0d | 0xff8d => {
                if alt {
                    return;
                }
                self.open_selection();
            }
            0xff08 => self.up(),             // BackSpace
            0xff52 if alt => self.up(),      // Alt+Up
            0xff51 if alt => self.back(),    // Alt+Left
            0xff53 if alt => self.forward(), // Alt+Right
            0xff51 if n > 0 => self.set_cursor(cur.saturating_sub(1), shift, false),
            0xff53 if n > 0 => self.set_cursor((cur + 1).min(n - 1), shift, false),
            0xff52 if n > 0 => self.set_cursor(cur.saturating_sub(cols), shift, false),
            0xff54 if n > 0 => self.set_cursor((cur + cols).min(n - 1), shift, false),
            0xff55 if n > 0 => {
                let p = self.page();
                self.set_cursor(cur.saturating_sub(p), shift, false)
            }
            0xff56 if n > 0 => {
                let p = self.page();
                self.set_cursor((cur + p).min(n - 1), shift, false)
            }
            0xff50 if n > 0 => self.set_cursor(0, shift, false),
            0xff57 if n > 0 => self.set_cursor(n - 1, shift, false),
            0xffc2 => {
                // F5
                let a = self.active;
                self.load_tab(a, false);
            }
            0xffc1 => {
                // F4: a terminal here
                let p = self.tab().path.clone();
                spawn(
                    "kitty",
                    &[std::ffi::OsStr::new("--directory"), p.as_os_str()],
                    Some(&p),
                );
            }
            0xff09 | 0xfe20 if ctrl => {
                // Ctrl+Tab, Ctrl+Shift+Tab
                let k = self.tabs.len();
                let i = if shift {
                    (self.active + k - 1) % k
                } else {
                    (self.active + 1) % k
                };
                self.switch(i);
            }
            _ if ctrl => self.ctrl_key(sym, shift),
            _ => {
                if let Some(c) = keysym_char(sym) {
                    if !c.is_control() && !alt {
                        self.type_ahead(c);
                    }
                }
            }
        }
    }

    fn ctrl_key(&mut self, sym: u32, shift: bool) {
        match sym | 0x20 {
            0x74 => {
                // t
                let p = self.tab().path.clone();
                self.new_tab(p);
            }
            0x77 => {
                // w
                let a = self.active;
                self.close_tab(a);
            }
            0x71 => self.quit = true,              // q
            0x63 if !shift => self.act(Act::Copy), // c
            0x63 => self.act(Act::CopyPath),       // Ctrl+Shift+C: the paths
            0x78 => self.act(Act::Cut),            // x
            0x76 => self.act(Act::Paste),          // v
            0x7a => self.undo_last(),              // z
            0x6b => self.act(Act::Connect),        // k
            0x64 => self.act(Act::Bookmark),       // d
            0x68 => {
                // h
                self.hidden = !self.hidden;
                let a = self.active;
                self.load_tab(a, false);
                self.flash(if self.hidden {
                    "Hidden files shown".into()
                } else {
                    "Hidden files hidden".into()
                });
            }
            0x61 => self.select_all(), // a
            0x6c => {
                // l
                self.path_edit = Some(places::pretty(&self.tab().path));
                self.dirty = true;
            }
            0x72 => {
                // r
                let a = self.active;
                self.load_tab(a, false);
            }
            0x6e if shift => self.act(Act::NewFolder),
            _ => match sym {
                0x2b | 0x3d | 0xffab => self.set_zoom(self.zoom + 1), // + = KP_Add
                0x2d | 0xffad => self.set_zoom(self.zoom.saturating_sub(1)), // - KP_Subtract
                0x30 => self.set_zoom(ZOOM_DEFAULT),
                0x31 => self.set_view(View::Icons), // 1
                0x32 => self.set_view(View::List),  // 2
                0xff56 => {
                    let k = self.tabs.len();
                    self.switch((self.active + 1) % k)
                }
                0xff55 => {
                    let k = self.tabs.len();
                    self.switch((self.active + k - 1) % k)
                }
                _ => {}
            },
        }
    }

    fn set_view(&mut self, v: View) {
        if self.view != v {
            self.view = v;
            self.tab_mut().scroll = 0;
            self.dirty = true;
            self.layout_only();
            self.reveal();
        }
    }

    fn path_key(&mut self, sym: u32, ctrl: bool) {
        let Some(buf) = self.path_edit.as_mut() else {
            return;
        };
        match sym {
            0xff1b => self.path_edit = None,
            0xff08 => {
                if ctrl {
                    // Remove the last component.
                    let trimmed = buf.trim_end_matches('/');
                    let cut = trimmed.rfind('/').map(|i| i + 1).unwrap_or(0);
                    buf.truncate(cut);
                } else {
                    buf.pop();
                }
            }
            0xff09 => {
                // Tab: complete a folder name.
                let expanded = expand(buf);
                let (dir, prefix) = match expanded.rfind('/') {
                    Some(i) => (expanded[..=i].to_string(), expanded[i + 1..].to_string()),
                    None => ("./".into(), expanded.clone()),
                };
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    let mut names: Vec<String> = rd
                        .flatten()
                        .filter(|e| e.path().is_dir())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| {
                            n.starts_with(&prefix)
                                && (self.hidden || !n.starts_with('.') || prefix.starts_with('.'))
                        })
                        .collect();
                    names.sort_by(|a, b| dir::natural(a, b));
                    if let Some(first) = names.first() {
                        let common = names.iter().fold(first.clone(), |acc, n| {
                            acc.chars()
                                .zip(n.chars())
                                .take_while(|(a, b)| a == b)
                                .map(|(a, _)| a)
                                .collect()
                        });
                        let done = if names.len() == 1 {
                            format!("{common}/")
                        } else {
                            common
                        };
                        let base = buf
                            .rfind('/')
                            .map(|i| buf[..=i].to_string())
                            .unwrap_or_default();
                        *buf = format!("{base}{done}");
                    }
                }
            }
            0xff0d | 0xff8d => {
                let target = PathBuf::from(expand(buf));
                self.path_edit = None;
                if target.is_dir() {
                    self.go(target);
                } else if target.exists() {
                    spawn("xdg-open", &[target.as_os_str()], None);
                } else {
                    self.flash(format!("No such folder: {}", target.display()));
                }
            }
            _ => {
                if let Some(c) = keysym_char(sym) {
                    if !c.is_control() {
                        buf.push(c);
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn type_ahead(&mut self, c: char) {
        if self.typed_at.elapsed() > Duration::from_millis(900) {
            self.typed.clear();
        }
        self.typed_at = Instant::now();
        self.typed.extend(c.to_lowercase());
        let t = self.tab();
        let want = self.typed.clone();
        let start = t.cursor;
        let n = t.shown.len();
        // From the cursor on (a repeated letter cycles), then from the top.
        let found = (0..n)
            .map(|k| (start + if want.chars().count() == 1 { k + 1 } else { k }) % n.max(1))
            .find(|k| {
                t.entry(*k)
                    .is_some_and(|e| e.name.to_lowercase().starts_with(&want))
            });
        if let Some(k) = found {
            self.set_cursor(k, false, false);
        }
        self.dirty = true;
    }

    // ---------------------------------------------------------------- draw

    fn layout_only(&mut self) {
        let m = &self.m;
        let top = m.bar_h + if self.tabs.len() > 1 { m.tab_h } else { 0 };
        self.content = Rect::new(
            m.side_w + 1,
            top,
            self.cv.w - m.side_w - 1,
            self.cv.h - top - m.status_h,
        );
        self.cols = ((self.content.w - 2 * m.pad) / m.cell_w).max(1);
    }

    fn draw(&mut self) {
        self.hits.clear();
        self.layout_only();
        self.clamp_scroll();
        let p = self.p;
        let (w, h) = (self.cv.w, self.cv.h);
        self.cv.reset_clip();
        self.cv.fill(Rect::new(0, 0, w, h), p.background);
        self.draw_sidebar();
        self.draw_bar();
        if self.tabs.len() > 1 {
            self.draw_tabs();
        }
        self.draw_content();
        self.draw_status();
        self.draw_rename();
        self.draw_menu();
        self.draw_preview();
        self.draw_confirm();
        self.draw_connect();
    }

    fn draw_sidebar(&mut self) {
        let p = self.p;
        let m = &self.m;
        let (sw, h) = (m.side_w, self.cv.h);
        self.cv.fill(Rect::new(0, 0, sw, h), p.surface);
        self.cv.fill(Rect::new(sw, 0, 1, h), p.border);
        // The pixel logo heads the panel.
        let lp = (m.s * 3.0).round().max(2.0) as i32;
        let lx = m.pad * 2;
        let ly = (m.bar_h - 7 * lp) / 2;
        crate::logo::word(
            &mut self.cv,
            lx,
            ly,
            "NIRUFM",
            lp,
            p.accent,
            Some(blend(p.surface, p.accent, 0.25)),
        );
        let mut y = m.bar_h + m.pad / 2 - self.side_scroll;
        let row = (m.font.height + m.pad).max(m.small_icon + m.pad / 2);
        // The list scrolls under the logo row.
        self.cv.set_clip(Rect::new(0, m.bar_h, sw, h - m.bar_h));
        let cur = self.tab().path.clone();
        let mut section = "";
        let (font, small, pad, si) = (m.font, m.small, m.pad, m.small_icon);
        for (i, pl) in self.places.clone().iter().enumerate() {
            if pl.section != section {
                section = pl.section;
                y += pad / 2;
                self.text
                    .draw(&mut self.cv, pad * 2, y, section, small, p.muted);
                y += small.height + pad / 2;
            }
            let r = Rect::new(pad / 2, y, sw - pad, row);
            if cur == pl.path {
                self.cv.fill(r, p.selected);
                self.cv.fill(
                    Rect::new(r.x, r.y, (2.0 * self.m.s).max(2.0) as i32, r.h),
                    p.accent,
                );
            }
            let names = [pl.icon.to_string(), "folder".to_string()];
            if let Some(img) = self.icons.get(&names, si) {
                self.cv.image(pad * 2, y + (row - img.h) / 2, &img, 255);
            }
            let tx = pad * 3 + si;
            let eject = (pl.dev.is_some() || (pl.uri.is_some() && !pl.bookmark)) && pl.mounted;
            let ew = if eject {
                self.text.width("⏏", font) + pad
            } else {
                0
            };
            let label = self.text.ellipsize(&pl.label, font, sw - tx - pad - ew);
            let col = if cur == pl.path && pl.mounted {
                p.foreground
            } else if !pl.mounted {
                p.muted
            } else {
                blend(p.muted, p.foreground, 0.45)
            };
            if eject {
                let ex = sw - pad - ew;
                self.text.draw(
                    &mut self.cv,
                    ex,
                    y + (row - font.height) / 2,
                    "⏏",
                    font,
                    p.muted,
                );
                self.hits
                    .push((Rect::new(ex - pad / 2, y, ew + pad / 2, row), Hit::Eject(i)));
            }
            self.text.draw(
                &mut self.cv,
                tx,
                y + (row - font.height) / 2,
                &label,
                font,
                col,
            );
            let place_hit = (r, Hit::Place(i));
            // The eject button sits on the row: it is pushed after it.
            if let Some(pos) = self
                .hits
                .iter()
                .rposition(|(_, h)| matches!(h, Hit::Eject(k) if *k == i))
            {
                self.hits.insert(pos, place_hit);
            } else {
                self.hits.push(place_hit);
            }
            y += row;
        }
        self.side_total = y + self.side_scroll + pad;
        self.cv.reset_clip();
        // Rows scrolled under the logo row cannot be clicked.
        let top = self.m.bar_h;
        for (r, h) in self.hits.iter_mut() {
            if matches!(h, Hit::Place(_) | Hit::Eject(_)) && r.y < top {
                *r = r.intersect(&Rect::new(0, top, sw, self.cv.h));
            }
        }
    }

    fn draw_bar(&mut self) {
        let p = self.p;
        let (pad, font, bar_h, sw) = (self.m.pad, self.m.font, self.m.bar_h, self.m.side_w);
        let w = self.cv.w;
        let bar = Rect::new(sw + 1, 0, w - sw - 1, bar_h);
        self.cv.fill(bar, p.background);
        self.cv
            .fill(Rect::new(bar.x, bar_h - 1, bar.w, 1), p.border);
        let mut x = bar.x + pad;
        let ty = (bar_h - font.height) / 2;
        let can_back = !self.tab().back.is_empty();
        let can_fwd = !self.tab().forward.is_empty();
        for (label, hit, on) in [
            ("‹", Hit::Back, can_back),
            ("›", Hit::Forward, can_fwd),
            ("↑", Hit::Up, self.tab().path.parent().is_some()),
        ] {
            let bw = font.height + pad;
            let r = Rect::new(x, (bar_h - bw) / 2, bw, bw);
            let tw = self.text.width(label, font);
            self.text.draw(
                &mut self.cv,
                r.x + (bw - tw) / 2,
                ty,
                label,
                font,
                if on { p.foreground } else { p.border },
            );
            if on {
                self.hits.push((r, hit));
            }
            x += bw;
        }
        x += pad;
        if let Some(buf) = self.path_edit.clone() {
            let r = Rect::new(x, pad / 2, bar.x + bar.w - x - pad, bar_h - pad);
            self.cv.fill(r, p.surface);
            self.cv.frame(r, p.accent);
            let shown = format!("{buf}▏");
            let tw = self.text.width(&shown, font);
            let start = (tw - (r.w - 2 * pad)).max(0);
            self.cv.set_clip(r.inset(1));
            self.text.draw(
                &mut self.cv,
                r.x + pad - start,
                ty,
                &shown,
                font,
                p.foreground,
            );
            self.cv.reset_clip();
            return;
        }
        // Breadcrumbs: ~ / Documents / Work. The last one is current.
        let path = self.tab().path.clone();
        let home = theme::home();
        let mut crumbs: Vec<(String, PathBuf)> = Vec::new();
        let mut acc = PathBuf::new();
        let under_home = path.starts_with(&home) && home != Path::new("/");
        if under_home {
            crumbs.push(("~".into(), home.clone()));
            acc = home.clone();
            for c in path.strip_prefix(&home).unwrap().components() {
                acc.push(c);
                crumbs.push((c.as_os_str().to_string_lossy().into_owned(), acc.clone()));
            }
        } else {
            crumbs.push(("/".into(), PathBuf::from("/")));
            acc.push("/");
            for c in path.components().skip(1) {
                acc.push(c);
                crumbs.push((c.as_os_str().to_string_lossy().into_owned(), acc.clone()));
            }
        }
        let sep = " / ";
        let avail = bar.x + bar.w - x - pad;
        // Leading crumbs give way to "…" when the path is long.
        let widths: Vec<i32> = crumbs
            .iter()
            .map(|(n, _)| self.text.width(n, font))
            .collect();
        let sepw = self.text.width(sep, font);
        let mut first = 0;
        while first + 1 < crumbs.len() {
            let total: i32 = widths[first..].iter().sum::<i32>()
                + sepw * (crumbs.len() - first) as i32
                + if first > 0 { sepw * 2 } else { 0 };
            if total <= avail {
                break;
            }
            first += 1;
        }
        if first > 0 {
            x += self.text.draw(&mut self.cv, x, ty, "…", font, p.muted);
            x += self.text.draw(&mut self.cv, x, ty, sep, font, p.border);
        }
        let last = crumbs.len() - 1;
        for (k, (name, target)) in crumbs.iter().enumerate().skip(first) {
            let col = if k == last { p.foreground } else { p.muted };
            let label = if k == last {
                self.text
                    .ellipsize(name, font, (bar.x + bar.w - x - pad).max(0))
            } else {
                name.clone()
            };
            let tw = self.text.draw(&mut self.cv, x, ty, &label, font, col);
            self.hits.push((
                Rect::new(x - 2, 0, tw + 4, bar_h),
                Hit::Crumb(target.clone()),
            ));
            x += tw;
            if k != last && !(k == 0 && name == "/") {
                x += self.text.draw(&mut self.cv, x, ty, sep, font, p.border);
            }
        }
        // Clicking the empty part of the bar edits the path.
        let _ = bar;
    }

    fn draw_tabs(&mut self) {
        let p = self.p;
        let (pad, font, small, tab_h, bar_h, sw) = (
            self.m.pad,
            self.m.font,
            self.m.small,
            self.m.tab_h,
            self.m.bar_h,
            self.m.side_w,
        );
        let area = Rect::new(sw + 1, bar_h, self.cv.w - sw - 1, tab_h);
        self.cv.fill(area, p.surface);
        self.cv
            .fill(Rect::new(area.x, area.y + area.h - 1, area.w, 1), p.border);
        let n = self.tabs.len() as i32;
        let tw = ((area.w - pad) / n).min((220.0 * self.m.s) as i32);
        for i in 0..self.tabs.len() {
            let r = Rect::new(area.x + pad / 2 + i as i32 * tw, area.y, tw - 2, tab_h - 1);
            let active = i == self.active;
            if active {
                self.cv.fill(r, p.background);
                self.cv.fill(
                    Rect::new(r.x, r.y, r.w, (2.0 * self.m.s).max(2.0) as i32),
                    p.accent,
                );
            }
            let title = self.tabs[i].title();
            let xw = self.text.width("×", small) + pad;
            let label = self.text.ellipsize(&title, font, r.w - 2 * pad - xw);
            self.text.draw(
                &mut self.cv,
                r.x + pad,
                r.y + (tab_h - font.height) / 2,
                &label,
                font,
                if active { p.foreground } else { p.muted },
            );
            let cx = r.x + r.w - xw;
            self.text.draw(
                &mut self.cv,
                cx,
                r.y + (tab_h - small.height) / 2,
                "×",
                small,
                p.muted,
            );
            self.hits.push((r, Hit::Tab(i)));
            self.hits
                .push((Rect::new(cx - pad / 2, r.y, xw, r.h), Hit::TabClose(i)));
        }
    }

    fn draw_content(&mut self) {
        let p = self.p;
        let c = self.content;
        self.hits.push((c, Hit::Content));
        self.cv.set_clip(c);
        if let Some(err) = self.tab().error.clone() {
            let tag = if err.contains("os error 2") {
                "NOT FOUND"
            } else {
                "NO ACCESS"
            };
            crate::logo::splash(&mut self.cv, c, &p, tag);
            let small = self.m.small;
            let tw = self.text.width(&err, small);
            self.text.draw(
                &mut self.cv,
                c.x + (c.w - tw) / 2,
                c.y + c.h / 2 + c.h / 5,
                &err,
                small,
                p.error,
            );
            self.cv.reset_clip();
            return;
        }
        if self.tab().shown.is_empty() {
            let tag = if self.tab().all.is_empty() {
                "EMPTY FOLDER"
            } else {
                "ONLY HIDDEN FILES"
            };
            crate::logo::splash(&mut self.cv, c, &p, tag);
            self.cv.reset_clip();
            return;
        }
        match self.view {
            View::Icons => self.draw_icons(),
            View::List => self.draw_list(),
        }
        self.draw_scrollbar();
        self.cv.reset_clip();
    }

    fn icon_for(&mut self, e: &Entry, size: i32) -> Option<std::rc::Rc<crate::canvas::Image>> {
        let names: Vec<String> = if e.is_dir {
            vec![folder_icon(&e.path).to_string(), "folder".into()]
        } else if e.executable() && e.mime == "application/octet-stream" {
            vec![
                "application-x-executable".into(),
                "text-x-script".into(),
                "unknown".into(),
            ]
        } else {
            self.mime.icon_names(&e.mime)
        };
        self.icons.get(&names, size)
    }

    fn draw_icons(&mut self) {
        let p = self.p;
        let c = self.content;
        let (pad, font, icon, cell_w, cell_h) = (
            self.m.pad,
            self.m.font,
            self.m.icon,
            self.m.cell_w,
            self.m.cell_h,
        );
        let n = self.tab().shown.len();
        let cols = self.cols.max(1) as usize;
        let scroll = self.tab().scroll;
        let first_row = ((scroll - pad).max(0) / cell_h) as usize;
        let last_row = ((scroll + c.h) / cell_h + 1) as usize;
        let (cursor, hover) = (self.tab().cursor, self.hover);
        for i in first_row * cols..((last_row + 1) * cols).min(n) {
            let r = self.item_rect(i);
            let Some(e) = self.tab().entry(i).cloned() else {
                continue;
            };
            let sel = self.tab().selected[i];
            let inner = r.inset((2.0 * self.m.s) as i32);
            if sel {
                self.cv.fill(inner, p.selected);
            } else if hover == Some(i) {
                self.cv.fill(inner, blend(p.background, p.surface, 0.9));
            }
            if i == cursor {
                self.cv.frame(inner, if sel { p.accent } else { p.border });
            }
            let thumb = self.thumb_for(&e);
            if let Some(img) = thumb {
                // A thin frame makes photos read as pictures, not icons.
                let (tx, ty) = (r.x + (r.w - img.w) / 2, r.y + pad + (icon - img.h));
                self.cv
                    .frame(Rect::new(tx - 1, ty - 1, img.w + 2, img.h + 2), p.border);
                self.cv.image(tx, ty, &img, 255);
            } else if let Some(img) = self.icon_for(&e, icon) {
                let alpha = if e.hidden() { 150 } else { 255 };
                self.cv
                    .image(r.x + (r.w - img.w) / 2, r.y + pad, &img, alpha);
            }
            let lines = self.text.wrap(&e.label, font, cell_w - pad, 2);
            let mut ty = r.y + pad + icon + pad / 2;
            let col = if e.hidden() { p.muted } else { p.foreground };
            for l in lines {
                let tw = self.text.width(&l, font);
                self.text
                    .draw(&mut self.cv, r.x + (r.w - tw) / 2, ty, &l, font, col);
                ty += font.height;
            }
            if e.is_link {
                if let Some(img) = self.icons.named("emblem-symbolic-link", (icon / 3).max(12)) {
                    self.cv.image(
                        r.x + (r.w + icon) / 2 - img.w,
                        r.y + pad + icon - img.h,
                        &img,
                        255,
                    );
                }
            }
            self.hits.push((r.intersect(&c), Hit::Item(i)));
        }
    }

    fn draw_list(&mut self) {
        let p = self.p;
        let c = self.content;
        let (pad, font, small, row_h, si) = (
            self.m.pad,
            self.m.font,
            self.m.small,
            self.m.row_h,
            self.m.small_icon,
        );
        let size_w = self.text.width("0000 MB", small) + 2 * pad;
        let date_w = self.text.width("2026-11-21 18:04", small) + 2 * pad;
        let type_w = (c.w / 5).clamp((90.0 * self.m.s) as i32, (240.0 * self.m.s) as i32);
        let name_w = c.w - size_w - date_w - type_w - pad;
        let cols = [
            ("Name", SortKey::Name, c.x + pad, name_w),
            ("Size", SortKey::Size, c.x + pad + name_w, size_w),
            (
                "Modified",
                SortKey::Modified,
                c.x + pad + name_w + size_w,
                date_w,
            ),
            (
                "Type",
                SortKey::Type,
                c.x + pad + name_w + size_w + date_w,
                type_w,
            ),
        ];
        let n = self.tab().shown.len();
        let scroll = self.tab().scroll;
        let first = (scroll / row_h).max(0) as usize;
        let last = ((scroll + c.h) / row_h + 1) as usize;
        let (cursor, hover) = (self.tab().cursor, self.hover);
        for i in first..last.min(n) {
            let r = self.item_rect(i);
            let Some(e) = self.tab().entry(i).cloned() else {
                continue;
            };
            let sel = self.tab().selected[i];
            if sel {
                self.cv.fill(r, p.selected);
            } else if hover == Some(i) {
                self.cv.fill(r, blend(p.background, p.surface, 0.9));
            } else if i % 2 == 1 {
                self.cv.fill(r, blend(p.background, p.surface, 0.45));
            }
            if i == cursor {
                self.cv.frame(r, if sel { p.accent } else { p.border });
            }
            if let Some(img) = self.icon_for(&e, si) {
                self.cv.image(
                    c.x + pad,
                    r.y + (row_h - img.h) / 2,
                    &img,
                    if e.hidden() { 150 } else { 255 },
                );
            }
            let tx = c.x + pad * 2 + si;
            let name = self.text.ellipsize(&e.label, font, name_w - si - 2 * pad);
            self.text.draw(
                &mut self.cv,
                tx,
                r.y + (row_h - font.height) / 2,
                &name,
                font,
                if e.hidden() { p.muted } else { p.foreground },
            );
            let ty = r.y + (row_h - small.height) / 2;
            let size = if e.is_dir {
                String::new()
            } else {
                dir::human_size(e.size)
            };
            let sw = self.text.width(&size, small);
            self.text.draw(
                &mut self.cv,
                cols[1].2 + cols[1].3 - pad - sw,
                ty,
                &size,
                small,
                p.muted,
            );
            let date = dir::human_time(e.mtime);
            self.text
                .draw(&mut self.cv, cols[2].2 + pad, ty, &date, small, p.muted);
            let kind = if e.is_dir {
                "folder".to_string()
            } else {
                e.mime.clone()
            };
            let kind = self.text.ellipsize(&kind, small, type_w - pad);
            self.text
                .draw(&mut self.cv, cols[3].2 + pad, ty, &kind, small, p.muted);
            self.hits.push((r.intersect(&c), Hit::Item(i)));
        }
        // The header stays on top while the rows scroll under it.
        let hr = Rect::new(c.x, c.y, c.w, row_h);
        self.cv.fill(hr, p.surface);
        self.cv
            .fill(Rect::new(c.x, c.y + row_h - 1, c.w, 1), p.border);
        for (label, key, x, w) in cols {
            let mark = if self.sort == key {
                if self.reverse {
                    " ▾"
                } else {
                    " ▴"
                }
            } else {
                ""
            };
            let text = format!("{label}{mark}");
            let col = if self.sort == key {
                p.foreground
            } else {
                p.muted
            };
            let tx = if key == SortKey::Size {
                x + w - pad - self.text.width(&text, small)
            } else if key == SortKey::Name {
                x + si + pad
            } else {
                x + pad
            };
            self.text.draw(
                &mut self.cv,
                tx,
                c.y + (row_h - small.height) / 2,
                &text,
                small,
                col,
            );
            self.hits
                .push((Rect::new(x, c.y, w, row_h), Hit::Header(key)));
        }
    }

    fn draw_scrollbar(&mut self) {
        let total = self.content_height();
        let c = self.content;
        if total <= c.h {
            return;
        }
        let w = (4.0 * self.m.s).max(3.0) as i32;
        let track = Rect::new(c.x + c.w - w - 2, c.y + 2, w, c.h - 4);
        let th = (track.h as i64 * c.h as i64 / total as i64).max(20) as i32;
        let ty = track.y
            + ((track.h - th) as i64 * self.tab().scroll as i64 / (total - c.h).max(1) as i64)
                as i32;
        self.cv.fill(
            Rect::new(track.x, ty, w, th),
            blend(self.p.border, self.p.muted, 0.5),
        );
    }

    fn draw_status(&mut self) {
        let p = self.p;
        let (pad, small, status_h, sw) = (self.m.pad, self.m.small, self.m.status_h, self.m.side_w);
        let r = Rect::new(sw + 1, self.cv.h - status_h, self.cv.w - sw - 1, status_h);
        self.cv.fill(r, p.surface);
        self.cv.fill(Rect::new(r.x, r.y, r.w, 1), p.border);
        let t = self.tab();
        let (n, nsel) = (t.shown.len(), t.selected.iter().filter(|s| **s).count());
        let sel_bytes: u64 = t
            .selected
            .iter()
            .enumerate()
            .filter(|(_, s)| **s)
            .filter_map(|(i, _)| t.entry(i))
            .filter(|e| !e.is_dir)
            .map(|e| e.size)
            .sum();
        let mut left = format!("{n} item{}", if n == 1 { "" } else { "s" });
        if nsel > 0 {
            left.push_str(&format!(" · {nsel} selected"));
            if sel_bytes > 0 {
                left.push_str(&format!(" ({})", dir::human_size(sel_bytes)));
            }
        }
        if let Some((free, _)) = dir::space(&t.path) {
            left.push_str(&format!(" · {} free", dir::human_size(free)));
        }
        if self.hidden {
            left.push_str(" · hidden shown");
        }
        let ty = r.y + (status_h - small.height) / 2;
        let msg = self.message.as_ref().map(|(m, _)| m.clone());
        if let Some(run) = &self.running {
            // Progress: a pixel bar and the item being worked on.
            let pct = (run.done * 100)
                .checked_div(run.total)
                .unwrap_or(0)
                .min(100);
            let label = format!("{} {pct}% · {} · Esc cancels", run.verb, run.current);
            let bw = (120.0 * self.m.s) as i32;
            let bh = (small.height / 2).max(4);
            let by = r.y + (status_h - bh) / 2;
            self.cv.fill(Rect::new(r.x + pad, by, bw, bh), p.border);
            self.cv.fill(
                Rect::new(r.x + pad, by, (bw as u64 * pct / 100) as i32, bh),
                p.accent,
            );
            let label = self.text.ellipsize(&label, small, r.w / 2);
            self.text.draw(
                &mut self.cv,
                r.x + 2 * pad + bw,
                ty,
                &label,
                small,
                p.foreground,
            );
        } else if let Some(m) = msg {
            let col = if self.error { p.error } else { p.accent };
            self.text.draw(&mut self.cv, r.x + pad, ty, &m, small, col);
        } else if !self.typed.is_empty() && self.typed_at.elapsed() < Duration::from_millis(900) {
            let s = format!("find: {}", self.typed);
            self.text
                .draw(&mut self.cv, r.x + pad, ty, &s, small, p.accent);
        } else {
            self.text
                .draw(&mut self.cv, r.x + pad, ty, &left, small, p.muted);
        }
        let right = match self.view {
            View::Icons => "Ctrl+2 list",
            View::List => "Ctrl+1 icons",
        };
        let rw = self.text.width(right, small);
        self.text.draw(
            &mut self.cv,
            r.x + r.w - rw - pad,
            ty,
            right,
            small,
            p.border,
        );
    }
}

impl App {
    fn selection_paths(&self) -> Vec<PathBuf> {
        let t = self.tab();
        let v: Vec<PathBuf> = t
            .selected
            .iter()
            .enumerate()
            .filter(|(_, s)| **s)
            .filter_map(|(i, _)| t.entry(i).map(|e| e.path.clone()))
            .collect();
        if v.is_empty() {
            t.entry(t.cursor)
                .filter(|_| !t.shown.is_empty())
                .map(|e| vec![e.path.clone()])
                .unwrap_or_default()
        } else {
            v
        }
    }

    fn in_trash(&self) -> bool {
        self.tab().path.starts_with(places::trash_files())
    }

    fn submit(&mut self, job: crate::ops::Job) {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if self.running.is_none() {
            self.running = Some(Running {
                verb: job.verb(),
                done: 0,
                total: 0,
                current: String::new(),
                cancel: cancel.clone(),
            });
        }
        self.pending += 1;
        let _ = self.worker.tx.send((job, cancel));
        self.dirty = true;
    }

    fn worker_messages(&mut self) {
        while let Ok(d) = self.thumbs.rx.try_recv() {
            let fitted = d.image.map(|img| {
                let b = self.m.icon;
                std::rc::Rc::new(crate::icons::scale_to_fit(&img, b, b))
            });
            self.thumb_img.insert(d.path, fitted);
            self.dirty = true;
        }
        while let Ok(pv) = self.preview_rx.try_recv() {
            if self.preview.as_ref().is_some_and(|cur| cur.path == pv.path) {
                self.preview = Some(pv);
                self.dirty = true;
            }
        }
        while let Ok(m) = self.rx.try_recv() {
            match m {
                crate::ops::Msg::Progress {
                    done,
                    total,
                    current,
                } => {
                    if let Some(r) = self.running.as_mut() {
                        r.done = done;
                        r.total = total;
                        r.current = current;
                    }
                }
                crate::ops::Msg::Done {
                    summary,
                    undo,
                    errors,
                    created,
                    open,
                    login,
                } => {
                    if let Some(uri) = login {
                        self.pending = self.pending.saturating_sub(1);
                        if self.pending == 0 {
                            self.running = None;
                        }
                        self.open_connect(
                            Some(uri),
                            Some("The server asks for a user name and password.".into()),
                        );
                        continue;
                    }
                    self.pending = self.pending.saturating_sub(1);
                    if self.pending == 0 {
                        self.running = None;
                    }
                    if let Some(u) = undo {
                        self.undo.push(u);
                        if self.undo.len() > 50 {
                            self.undo.remove(0);
                        }
                    }
                    self.select_after = created;
                    if let Some(dir) = open {
                        self.refresh_places();
                        self.go(dir);
                    } else if summary == "Safe to remove" {
                        self.refresh_places();
                    }
                    if let Some(first) = errors.first() {
                        let more = if errors.len() > 1 {
                            format!(" (+{} more)", errors.len() - 1)
                        } else {
                            String::new()
                        };
                        self.flash_error(format!("{first}{more}"));
                    } else {
                        self.flash(summary);
                    }
                    let a = self.active;
                    self.load_tab(a, false);
                    self.apply_select_after();
                }
            }
        }
        self.dirty = true;
    }

    fn apply_select_after(&mut self) {
        if self.select_after.is_empty() {
            return;
        }
        let want = std::mem::take(&mut self.select_after);
        let t = self.tab_mut();
        let mut first = None;
        for (k, idx) in t.shown.iter().enumerate() {
            let hit = want.contains(&t.all[*idx].path);
            t.selected[k] = hit;
            if hit && first.is_none() {
                first = Some(k);
            }
        }
        if let Some(f) = first {
            t.cursor = f;
            t.anchor = f;
            self.reveal();
        }
    }

    fn flash_error(&mut self, s: String) {
        self.message = Some((s, Instant::now()));
        self.error = true;
        self.dirty = true;
    }

    fn act(&mut self, a: Act) {
        self.error = false;
        let here = self.tab().path.clone();
        let sel = self.selection_paths();
        match a {
            Act::Open => self.open_selection(),
            Act::OpenTab => {
                for p in sel.into_iter().filter(|p| p.is_dir()) {
                    self.new_tab(p);
                }
            }
            Act::Copy | Act::Cut => {
                if sel.is_empty() {
                    return;
                }
                let cut = a == Act::Cut;
                let n = sel.len();
                self.clip = Some((sel, cut));
                let _ = self.x.own_clipboard();
                self.flash(format!(
                    "{} {n} item{}",
                    if cut { "Cut" } else { "Copied" },
                    if n == 1 { "" } else { "s" }
                ));
            }
            Act::CopyPath => {
                if sel.is_empty() {
                    return;
                }
                // Paths as text for a terminal; pasting here still copies.
                self.clip = Some((sel, false));
                let _ = self.x.own_clipboard();
                self.flash("Copied the paths".into());
            }
            Act::Paste => {
                if self.x.owns_clipboard() {
                    if let Some((files, cut)) = self.clip.clone() {
                        self.paste_files(files, cut);
                    }
                } else {
                    let _ = self.x.request_clipboard(self.x.atoms.gnome_copied);
                }
            }
            Act::Rename => {
                let t = self.tab();
                if let Some(e) = t.entry(t.cursor) {
                    let name = e.name.clone();
                    let c = t.cursor;
                    self.rename = Some((c, name));
                    self.dirty = true;
                }
            }
            Act::Trash => {
                if sel.is_empty() {
                    return;
                }
                self.submit(crate::ops::Job::Trash { paths: sel });
            }
            Act::Delete => {
                if sel.is_empty() {
                    return;
                }
                let n = sel.len();
                let what = if n == 1 {
                    format!(
                        "\"{}\"",
                        sel[0].file_name().unwrap_or_default().to_string_lossy()
                    )
                } else {
                    format!("{n} items")
                };
                self.confirm = Some(Confirm {
                    text: format!("Delete {what} permanently? It cannot be undone.  Enter: delete · Esc: keep"),
                    job: crate::ops::Job::Delete { paths: sel },
                });
                self.dirty = true;
            }
            Act::Restore => {
                let pairs: Vec<_> = sel
                    .iter()
                    .filter_map(|f| {
                        crate::ops::trash_origin(f).map(|(info, orig)| (f.clone(), info, orig))
                    })
                    .collect();
                if !pairs.is_empty() {
                    self.submit(crate::ops::Job::Restore { pairs });
                }
            }
            Act::EmptyTrash => {
                self.confirm = Some(Confirm {
                    text: "Empty the trash? Everything in it is deleted for good.  Enter: empty · Esc: keep".into(),
                    job: crate::ops::Job::Delete {
                        // The files and their .trashinfo records together.
                        paths: ["files", "info"]
                            .iter()
                            .filter_map(|d| std::fs::read_dir(places::trash_files().with_file_name(d)).ok())
                            .flat_map(|rd| rd.flatten().map(|e| e.path()))
                            .collect(),
                    },
                });
                self.dirty = true;
            }
            Act::Extract => {
                for p in sel
                    .into_iter()
                    .filter(|p| crate::archive::is_archive(&p.to_string_lossy()))
                {
                    self.submit(crate::ops::Job::Extract {
                        archive: p,
                        dst: here.clone(),
                    });
                }
            }
            Act::PackZip | Act::PackTar => {
                if sel.is_empty() {
                    return;
                }
                let ext = if a == Act::PackZip { ".zip" } else { ".tar.gz" };
                let base = if sel.len() == 1 {
                    let n = sel[0]
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    crate::ops::split_ext(&n, sel[0].is_dir()).0
                } else {
                    here.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "archive".into())
                };
                let out = crate::ops::free_name(&here, Path::new(&format!("{base}{ext}")));
                self.submit(crate::ops::Job::Compress { paths: sel, out });
            }
            Act::NewFolder => {
                let p = crate::ops::free_name(&here, Path::new("New folder"));
                match std::fs::create_dir(&p) {
                    Ok(()) => {
                        self.undo
                            .push(crate::ops::Undo::TrashCreated(vec![p.clone()]));
                        let a = self.active;
                        self.load_tab(a, false);
                        self.select_after = vec![p];
                        self.apply_select_after();
                        self.act(Act::Rename);
                    }
                    Err(e) => self.flash_error(format!("Could not create the folder: {e}")),
                }
            }
            Act::Terminal => spawn(
                "kitty",
                &[std::ffi::OsStr::new("--directory"), here.as_os_str()],
                Some(&here),
            ),
            Act::Bookmark => match crate::remote::add_bookmark(&here) {
                Ok(true) => {
                    self.flash(format!("Bookmarked {}", places::pretty(&here)));
                    self.refresh_places();
                }
                Ok(false) => self.flash("Already bookmarked".into()),
                Err(e) => self.flash_error(format!("Could not bookmark: {e}")),
            },
            Act::RemoveBookmark => {
                if let Some(pl) = self
                    .menu_place
                    .take()
                    .and_then(|i| self.places.get(i).cloned())
                {
                    let uri = pl
                        .uri
                        .clone()
                        .unwrap_or_else(|| crate::x::file_uri(&pl.path));
                    let _ = crate::remote::remove_bookmark(&uri);
                    self.refresh_places();
                }
            }
            Act::Connect => self.open_connect(None, None),
            Act::NcPublic | Act::NcInternal | Act::NcShare | Act::NcBrowser => {
                let Some(p) = sel.first() else { return };
                let (cmd, done) = match a {
                    Act::NcPublic => (
                        crate::nextcloud::Command::PublicLink,
                        "Nextcloud copies the public link",
                    ),
                    Act::NcInternal => (
                        crate::nextcloud::Command::InternalLink,
                        "Nextcloud copies the internal link",
                    ),
                    Act::NcShare => (
                        crate::nextcloud::Command::Share,
                        "Opening Nextcloud sharing",
                    ),
                    _ => (
                        crate::nextcloud::Command::OpenInBrowser,
                        "Opening in the browser",
                    ),
                };
                match crate::nextcloud::send(cmd, p) {
                    Ok(()) => self.flash(done.into()),
                    Err(e) => self.flash_error(format!("Nextcloud: {e}")),
                }
            }
            Act::Hidden => {
                self.hidden = !self.hidden;
                let a = self.active;
                self.load_tab(a, false);
            }
        }
    }

    fn paste_files(&mut self, files: Vec<PathBuf>, cut: bool) {
        let files: Vec<PathBuf> = files
            .into_iter()
            .filter(|p| std::fs::symlink_metadata(p).is_ok())
            .collect();
        if files.is_empty() {
            self.flash_error("Nothing to paste".into());
            return;
        }
        let dst = self.tab().path.clone();
        if cut {
            self.clip = None;
            self.submit(crate::ops::Job::Move { srcs: files, dst });
        } else {
            self.submit(crate::ops::Job::Copy { srcs: files, dst });
        }
    }

    /// The answer to our clipboard request from another program.
    fn pasted(&mut self, target: u32, property: u32) {
        let a = &self.x.atoms;
        if property == x11rb::NONE {
            // No files in that format: try the plain URI list once.
            if target == a.gnome_copied {
                let _ = self.x.request_clipboard(a.uri_list);
            } else {
                self.flash_error("The clipboard has no files".into());
            }
            return;
        }
        let Some(data) = self.x.take_selection() else {
            return;
        };
        let (files, cut) = crate::x::parse_uris(&data);
        self.paste_files(files, cut);
    }

    fn undo_last(&mut self) {
        let Some(u) = self.undo.pop() else {
            self.flash("Nothing to undo".into());
            return;
        };
        match u {
            crate::ops::Undo::MoveBack(pairs) => {
                // Each item goes back to its own folder.
                let mut n = 0;
                for (now, orig) in pairs {
                    if std::fs::symlink_metadata(&orig).is_err()
                        && std::fs::rename(&now, &orig).is_ok()
                    {
                        n += 1;
                    }
                }
                self.flash(format!(
                    "Undone: {n} item{} moved back",
                    if n == 1 { "" } else { "s" }
                ));
                let a = self.active;
                self.load_tab(a, false);
            }
            crate::ops::Undo::Untrash(v) => self.submit(crate::ops::Job::Restore { pairs: v }),
            crate::ops::Undo::TrashCreated(v) => {
                let v: Vec<PathBuf> = v
                    .into_iter()
                    .filter(|p| std::fs::symlink_metadata(p).is_ok())
                    .collect();
                if !v.is_empty() {
                    self.submit(crate::ops::Job::Trash { paths: v });
                }
            }
        }
    }

    fn rename_key(&mut self, sym: u32, ctrl: bool) {
        let Some((_, buf)) = self.rename.as_mut() else {
            return;
        };
        match sym {
            0xff1b => self.rename = None,
            0xff0d | 0xff8d => self.commit_rename(),
            0xff08 => {
                if ctrl {
                    buf.clear();
                } else {
                    buf.pop();
                }
            }
            _ => {
                if let Some(c) = keysym_char(sym) {
                    if !c.is_control() && c != '/' {
                        buf.push(c);
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn commit_rename(&mut self) {
        let Some((i, name)) = self.rename.take() else {
            return;
        };
        self.dirty = true;
        let Some(e) = self.tab().entry(i).cloned() else {
            return;
        };
        let name = name.trim().to_string();
        if name.is_empty() || name == e.name || name == "." || name == ".." {
            return;
        }
        let target = e.path.with_file_name(&name);
        if std::fs::symlink_metadata(&target).is_ok() {
            self.flash_error(format!("\"{name}\" already exists"));
            return;
        }
        match std::fs::rename(&e.path, &target) {
            Ok(()) => {
                self.undo.push(crate::ops::Undo::MoveBack(vec![(
                    target.clone(),
                    e.path.clone(),
                )]));
                let a = self.active;
                self.load_tab(a, false);
                self.select_after = vec![target];
                self.apply_select_after();
            }
            Err(err) => self.flash_error(format!("Could not rename: {err}")),
        }
    }

    fn open_menu(&mut self, x: i32, y: i32, on_items: bool) {
        let sel = self.selection_paths();
        let mut items: Vec<(String, Act, &'static str)> = Vec::new();
        let trash = self.in_trash();
        if on_items && !sel.is_empty() {
            items.push(("Open".into(), Act::Open, "Enter"));
            if sel.iter().any(|p| p.is_dir()) {
                items.push(("Open in a new tab".into(), Act::OpenTab, ""));
            }
            if trash {
                items.push(("Restore".into(), Act::Restore, ""));
                items.push(("Delete permanently".into(), Act::Delete, "Del"));
            } else {
                items.push(("Cut".into(), Act::Cut, "Ctrl+X"));
                items.push(("Copy".into(), Act::Copy, "Ctrl+C"));
                items.push(("Copy path".into(), Act::CopyPath, "Ctrl+Shift+C"));
                if sel.len() == 1 {
                    items.push(("Rename".into(), Act::Rename, "F2"));
                }
                if sel.len() == 1
                    && crate::nextcloud::in_sync_folder(&sel[0])
                    && crate::nextcloud::running()
                {
                    items.push(("Copy public link".into(), Act::NcPublic, "Nextcloud"));
                    items.push(("Copy internal link".into(), Act::NcInternal, "Nextcloud"));
                    items.push(("Share…".into(), Act::NcShare, "Nextcloud"));
                    items.push(("Open in Nextcloud".into(), Act::NcBrowser, "browser"));
                }
                if sel
                    .iter()
                    .any(|p| crate::archive::is_archive(&p.to_string_lossy()))
                {
                    items.push(("Unpack here".into(), Act::Extract, ""));
                }
                items.push(("Pack as .zip".into(), Act::PackZip, ""));
                items.push(("Pack as .tar.gz".into(), Act::PackTar, ""));
                items.push(("Move to the trash".into(), Act::Trash, "Del"));
                items.push(("Delete permanently".into(), Act::Delete, "Shift+Del"));
            }
        } else {
            if !trash {
                items.push(("Paste".into(), Act::Paste, "Ctrl+V"));
                items.push(("New folder".into(), Act::NewFolder, "Ctrl+Shift+N"));
            } else {
                items.push(("Empty the trash".into(), Act::EmptyTrash, ""));
            }
            items.push(("Terminal here".into(), Act::Terminal, "F4"));
            items.push((
                if self.hidden {
                    "Hide hidden files"
                } else {
                    "Show hidden files"
                }
                .into(),
                Act::Hidden,
                "Ctrl+H",
            ));
        }
        self.menu = Some(Menu {
            x,
            y,
            items,
            hover: None,
        });
        self.dirty = true;
    }

    fn draw_menu(&mut self) {
        let Some(m) = self.menu.as_ref() else { return };
        let p = self.p;
        let (font, small, pad) = (self.m.font, self.m.small, self.m.pad);
        let items = m.items.clone();
        let hover = m.hover;
        let row = font.height + pad;
        let label_w = items
            .iter()
            .map(|it| self.text.width(&it.0, font))
            .max()
            .unwrap_or(0);
        let key_w = items
            .iter()
            .map(|it| self.text.width(it.2, small))
            .max()
            .unwrap_or(0);
        let w = label_w + key_w + 5 * pad;
        let h = row * items.len() as i32 + pad;
        let x = m.x.min(self.cv.w - w - 2).max(0);
        let y = if m.y + h > self.cv.h {
            (m.y - h).max(0)
        } else {
            m.y
        };
        let r = Rect::new(x, y, w, h);
        self.hits
            .push((Rect::new(0, 0, self.cv.w, self.cv.h), Hit::MenuOutside));
        // A hard pixel shadow, the retro way.
        let sh = (3.0 * self.m.s) as i32;
        self.cv
            .tint(Rect::new(r.x + sh, r.y + sh, r.w, r.h), 0x000000, 110);
        self.cv.fill(r, p.surface);
        self.cv.frame(r, p.border);
        for (i, (label, _, key)) in items.iter().enumerate() {
            let ir = Rect::new(r.x + 1, r.y + pad / 2 + i as i32 * row, r.w - 2, row);
            if hover == Some(i) {
                self.cv.fill(ir, p.selected);
                self.cv.fill(
                    Rect::new(ir.x, ir.y, (2.0 * self.m.s).max(2.0) as i32, ir.h),
                    p.accent,
                );
            }
            let danger = label.starts_with("Delete") || label.starts_with("Empty");
            let col = if danger { p.error } else { p.foreground };
            self.text.draw(
                &mut self.cv,
                ir.x + 2 * pad,
                ir.y + (row - font.height) / 2,
                label,
                font,
                col,
            );
            let kw = self.text.width(key, small);
            self.text.draw(
                &mut self.cv,
                ir.x + ir.w - kw - pad * 2,
                ir.y + (row - small.height) / 2,
                key,
                small,
                p.muted,
            );
            self.hits.push((ir, Hit::MenuItem(i)));
        }
    }

    fn draw_confirm(&mut self) {
        let Some(c) = self.confirm.as_ref() else {
            return;
        };
        let p = self.p;
        let text = c.text.clone();
        let (font, pad) = (self.m.font, self.m.pad);
        let (main, keys) = text.split_once("  ").unwrap_or((&text, ""));
        let (main, keys) = (main.to_string(), keys.to_string());
        let w = (self
            .text
            .width(&main, font)
            .max(self.text.width(&keys, font))
            + 6 * pad)
            .min(self.cv.w - 4 * pad);
        let h = 2 * font.height + 5 * pad;
        let r = Rect::new((self.cv.w - w) / 2, (self.cv.h - h) / 2, w, h);
        self.cv
            .tint(Rect::new(0, 0, self.cv.w, self.cv.h), p.background, 140);
        let sh = (4.0 * self.m.s) as i32;
        self.cv
            .tint(Rect::new(r.x + sh, r.y + sh, r.w, r.h), 0x000000, 120);
        self.cv.fill(r, p.surface);
        self.cv.frame(r, p.error);
        let main = self.text.ellipsize(&main, font, w - 4 * pad);
        self.text.draw(
            &mut self.cv,
            r.x + 3 * pad,
            r.y + 2 * pad,
            &main,
            font,
            p.foreground,
        );
        self.text.draw(
            &mut self.cv,
            r.x + 3 * pad,
            r.y + 3 * pad + font.height,
            &keys,
            font,
            p.muted,
        );
    }

    /// The rename field over an item's label.
    fn draw_rename(&mut self) {
        let Some((i, buf)) = self.rename.clone() else {
            return;
        };
        let p = self.p;
        let (font, pad, icon) = (self.m.font, self.m.pad, self.m.icon);
        let r = self.item_rect(i);
        let fr = match self.view {
            View::Icons => Rect::new(
                r.x - pad,
                r.y + pad + icon + pad / 4,
                r.w + 2 * pad,
                font.height + pad / 2,
            ),
            View::List => Rect::new(
                r.x + 2 * pad + self.m.small_icon - pad / 2,
                r.y + 1,
                r.w / 2,
                r.h - 2,
            ),
        };
        let fr = Rect::new(
            fr.x.max(self.content.x),
            fr.y,
            fr.w.min(self.content.x + self.content.w - fr.x.max(self.content.x)),
            fr.h,
        );
        self.cv.set_clip(self.content);
        self.cv.fill(fr, p.background);
        self.cv.frame(fr, p.accent);
        let shown = format!("{buf}▏");
        let tw = self.text.width(&shown, font);
        let start = (tw - (fr.w - 2 * pad / 2)).max(0);
        self.cv.set_clip(fr.inset(1).intersect(&self.content));
        self.text.draw(
            &mut self.cv,
            fr.x + pad / 2 - start,
            fr.y + (fr.h - font.height) / 2,
            &shown,
            font,
            p.foreground,
        );
        self.cv.reset_clip();
    }
}

impl App {
    /// A thumbnail when there is one; asks for it the first time.
    fn thumb_for(&mut self, e: &Entry) -> Option<std::rc::Rc<crate::canvas::Image>> {
        if e.is_dir || !crate::thumbs::wanted(&e.mime, &self.thumb_types) || self.m.icon < 40 {
            return None;
        }
        if let Some(hit) = self.thumb_img.get(&e.path) {
            return hit.clone();
        }
        self.thumb_img.insert(e.path.clone(), None);
        // Thumbnails of very large files are left to the preview.
        if e.size > 512 * 1024 * 1024 {
            return None;
        }
        self.thumbs.request(crate::thumbs::Request {
            path: e.path.clone(),
            mime: e.mime.clone(),
            mtime: e.mtime,
            size: self.m.icon.max(1) as u32,
        });
        None
    }

    fn set_zoom(&mut self, z: usize) {
        let z = z.min(ZOOM.len() - 1);
        if z == self.zoom {
            return;
        }
        // Keep the item under the cursor in view.
        self.zoom = z;
        self.m = metrics(&self.text, self.font_px, self.m.s, z);
        self.thumbs.clear();
        self.thumb_img.clear();
        self.layout_only();
        self.reveal();
        self.dirty = true;
        self.flash(format!("Icons {} px", ZOOM[z] as i32));
    }

    fn toggle_preview(&mut self) {
        if self.preview.is_some() {
            self.preview = None;
            self.dirty = true;
            return;
        }
        self.start_preview();
    }

    fn start_preview(&mut self) {
        let t = self.tab();
        let Some(e) = t.entry(t.cursor).cloned() else {
            return;
        };
        self.preview = Some(Preview {
            path: e.path.clone(),
            image: None,
            lines: Vec::new(),
            note: None,
            ready: false,
        });
        self.dirty = true;
        let tx = self.preview_tx.clone();
        let wake = self.wake;
        let types = self.thumb_types.clone();
        let max = (self.cv.w.max(self.cv.h) as u32).clamp(400, 2048);
        std::thread::spawn(move || {
            let pv = make_preview(&e, &types, max);
            let _ = tx.send(pv);
            crate::ops::poke(wake);
        });
    }

    fn preview_key(&mut self, sym: u32) {
        let n = self.tab().shown.len();
        let cur = self.tab().cursor;
        let cols = if self.view == View::Icons {
            self.cols.max(1) as usize
        } else {
            1
        };
        let next = match sym {
            0x20 | 0xff1b => {
                self.preview = None;
                self.dirty = true;
                return;
            }
            0xff0d | 0xff8d => {
                self.preview = None;
                self.open(cur);
                return;
            }
            0xff51 => cur.saturating_sub(1),
            0xff53 => (cur + 1).min(n.saturating_sub(1)),
            0xff52 => cur.saturating_sub(cols),
            0xff54 => (cur + cols).min(n.saturating_sub(1)),
            _ => return,
        };
        if next != cur {
            self.set_cursor(next, false, false);
            self.start_preview();
        }
    }

    fn draw_preview(&mut self) {
        let Some(pv) = self.preview.as_ref() else {
            return;
        };
        let p = self.p;
        let (font, small, pad) = (self.m.font, self.m.small, self.m.pad);
        let path = pv.path.clone();
        let Some(e) = self.tab().all.iter().find(|e| e.path == path).cloned() else {
            return;
        };
        let full = Rect::new(0, 0, self.cv.w, self.cv.h);
        self.cv.tint(full, p.background, 170);
        let margin = (self.cv.w.min(self.cv.h) / 16).max(2 * pad);
        let r = Rect::new(
            margin,
            margin,
            self.cv.w - 2 * margin,
            self.cv.h - 2 * margin,
        );
        let sh = (5.0 * self.m.s) as i32;
        self.cv
            .tint(Rect::new(r.x + sh, r.y + sh, r.w, r.h), 0x000000, 120);
        self.cv.fill(r, p.surface);
        self.cv.frame(r, p.border);
        // Title row.
        let title_h = font.height + 2 * pad;
        self.cv.fill(
            Rect::new(r.x + 1, r.y + 1, r.w - 2, title_h),
            blend(p.surface, p.background, 0.5),
        );
        let name = self.text.ellipsize(&e.label, font, r.w - 4 * pad);
        self.text.draw(
            &mut self.cv,
            r.x + 2 * pad,
            r.y + pad,
            &name,
            font,
            p.foreground,
        );
        // Footer with facts and keys.
        let foot_h = small.height + 2 * pad;
        let fy = r.y + r.h - foot_h;
        self.cv.fill(Rect::new(r.x + 1, fy, r.w - 2, 1), p.border);
        let kind = if e.is_dir {
            "folder".to_string()
        } else {
            e.mime.clone()
        };
        let facts = if e.is_dir {
            format!("{kind} · {}", dir::human_time(e.mtime))
        } else {
            format!(
                "{} · {kind} · {}",
                dir::human_size(e.size),
                dir::human_time(e.mtime)
            )
        };
        self.text.draw(
            &mut self.cv,
            r.x + 2 * pad,
            fy + pad,
            &facts,
            small,
            p.muted,
        );
        let keys = "←→ next · Enter open · Space close";
        let kw = self.text.width(keys, small);
        self.text.draw(
            &mut self.cv,
            r.x + r.w - kw - 2 * pad,
            fy + pad,
            keys,
            small,
            p.border,
        );
        // Body.
        let body = Rect::new(
            r.x + 2 * pad,
            r.y + title_h + pad,
            r.w - 4 * pad,
            r.h - title_h - foot_h - 2 * pad,
        );
        let pv = self.preview.as_ref().unwrap();
        if let Some(img) = &pv.image {
            let fitted = crate::icons::scale_to_fit(img, body.w, body.h);
            let (ix, iy) = (
                body.x + (body.w - fitted.w) / 2,
                body.y + (body.h - fitted.h) / 2,
            );
            self.cv.fill(
                Rect::new(ix - 1, iy - 1, fitted.w + 2, fitted.h + 2),
                p.border,
            );
            self.cv
                .fill(Rect::new(ix, iy, fitted.w, fitted.h), 0x808080);
            self.cv.image(ix, iy, &fitted, 255);
        } else if !pv.lines.is_empty() {
            let lines = pv.lines.clone();
            self.cv.set_clip(body);
            let mut y = body.y;
            for l in lines {
                if y > body.y + body.h {
                    break;
                }
                let l = l.replace('\t', "    ");
                let l = self.text.ellipsize(&l, small, body.w);
                self.text
                    .draw(&mut self.cv, body.x, y, &l, small, p.foreground);
                y += small.height;
            }
            self.cv.reset_clip();
        } else {
            let note = if !pv.ready {
                "LOADING".to_string()
            } else {
                pv.note.clone().unwrap_or_else(|| "NO PREVIEW".into())
            };
            let big = (self.m.icon * 2).min(256);
            if let Some(img) = self.icon_for(&e, big) {
                self.cv.image(
                    body.x + (body.w - img.w) / 2,
                    body.y + body.h / 2 - img.h,
                    &img,
                    255,
                );
            }
            let tw = self.text.width(&note, small);
            self.text.draw(
                &mut self.cv,
                body.x + (body.w - tw) / 2,
                body.y + body.h / 2 + pad,
                &note,
                small,
                p.muted,
            );
        }
    }
}

/// Build a preview off the main thread: a picture (thumbnailers, PNG, PDF
/// through pdftoppm) or the first lines of a text file.
fn make_preview(e: &Entry, types: &std::collections::HashMap<String, ()>, max: u32) -> Preview {
    let mut pv = Preview {
        path: e.path.clone(),
        image: None,
        lines: Vec::new(),
        note: None,
        ready: true,
    };
    if e.is_dir {
        let n = std::fs::read_dir(&e.path).map(|rd| rd.count()).unwrap_or(0);
        pv.note = Some(format!("{n} ITEMS"));
        return pv;
    }
    let tmp = std::env::temp_dir().join(format!(
        "nirufm-preview-{}-{}.png",
        std::process::id(),
        crate::thumbs::md5_hex(e.path.to_string_lossy().as_bytes())
    ));
    if e.mime == "image/png" {
        if let Some(img) = std::fs::read(&e.path)
            .ok()
            .and_then(|d| crate::icons::decode_png(&d))
        {
            pv.image = Some(crate::icons::scale_to_fit(&img, max as i32, max as i32));
            return pv;
        }
    }
    if e.mime == "image/svg+xml" {
        if let Some(img) = std::fs::read(&e.path)
            .ok()
            .and_then(|d| crate::icons::render_svg(&d, (max as i32).min(1024)))
        {
            pv.image = Some(img);
            return pv;
        }
    }
    if e.mime == "application/pdf" && crate::archive::have("pdftoppm") {
        let prefix = tmp.with_extension("");
        let ok = std::process::Command::new("pdftoppm")
            .args([
                "-png",
                "-f",
                "1",
                "-l",
                "1",
                "-singlefile",
                "-scale-to",
                &max.to_string(),
            ])
            .arg(&e.path)
            .arg(&prefix)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            pv.image = std::fs::read(&tmp)
                .ok()
                .and_then(|d| crate::icons::decode_png(&d));
            let _ = std::fs::remove_file(&tmp);
            if pv.image.is_some() {
                return pv;
            }
        }
    }
    if types.contains_key(&e.mime) {
        // The system thumbnailer at full preview size; not cached.
        let th = crate::thumbs::thumbnailer_for(&e.mime);
        if let Some(exec) = th {
            if crate::thumbs::run_thumbnailer(
                &exec,
                &e.path,
                &crate::x::file_uri(&e.path),
                &tmp,
                max,
            ) {
                pv.image = std::fs::read(&tmp)
                    .ok()
                    .and_then(|d| crate::icons::decode_png(&d));
                let _ = std::fs::remove_file(&tmp);
                if pv.image.is_some() {
                    return pv;
                }
            }
        }
    }
    if e.mime == "application/pdf" {
        pv.note = Some("PDF PREVIEW NEEDS POPPLER-UTILS".into());
        return pv;
    }
    // Text: the start of the file, when it has no NUL bytes.
    let texty = e.mime.starts_with("text/")
        || [
            "json",
            "xml",
            "javascript",
            "x-shellscript",
            "toml",
            "yaml",
            "x-desktop",
            "sql",
        ]
        .iter()
        .any(|k| e.mime.contains(k))
        || e.size < 64 * 1024;
    if texty {
        if let Ok(mut f) = std::fs::File::open(&e.path) {
            use std::io::Read;
            let mut buf = vec![0u8; 16 * 1024];
            let n = f.read(&mut buf).unwrap_or(0);
            buf.truncate(n);
            if !buf.contains(&0) {
                pv.lines = String::from_utf8_lossy(&buf)
                    .lines()
                    .take(200)
                    .map(str::to_string)
                    .collect();
                if pv.lines.is_empty() {
                    pv.note = Some("EMPTY FILE".into());
                }
                return pv;
            }
        }
    }
    pv
}

impl App {
    fn open_connect(&mut self, uri: Option<String>, note: Option<String>) {
        let login = uri.is_some() && note.is_some();
        let uri = uri.unwrap_or_else(|| "sftp://".into());
        let user = std::env::var("USER").unwrap_or_default();
        self.connect = Some(ConnectForm {
            uri,
            user,
            pass: String::new(),
            field: if login { 2 } else { 0 },
            login,
            note,
        });
        self.dirty = true;
    }

    fn connect_key(&mut self, sym: u32, ctrl: bool, shift: bool) {
        let Some(f) = self.connect.as_mut() else {
            return;
        };
        let fields = if f.login { 3 } else { 1 };
        match sym {
            0xff1b => self.connect = None,
            0xff09 | 0xfe20 => {
                // Tab: complete a host from ~/.ssh/config, or move field.
                if f.field == 0 && f.uri.starts_with("sftp://") && !shift {
                    let typed = f.uri["sftp://".len()..].to_string();
                    let hosts = crate::remote::ssh_hosts();
                    let mut hits: Vec<&String> =
                        hosts.iter().filter(|h| h.starts_with(&typed)).collect();
                    hits.sort();
                    if let Some(h) = hits.first() {
                        f.uri = format!("sftp://{h}");
                    }
                } else if fields > 1 {
                    f.field = if shift {
                        (f.field + fields - 1) % fields
                    } else {
                        (f.field + 1) % fields
                    };
                }
            }
            0xff0d | 0xff8d => {
                let uri = f.uri.trim().to_string();
                if !uri.contains("://") || uri.ends_with("://") {
                    f.note = Some(
                        "Type an address like sftp://host, ftp://host or smb://host/share".into(),
                    );
                } else {
                    let login = f.login.then(|| (f.user.clone(), f.pass.clone()));
                    self.connect = None;
                    self.submit(crate::ops::Job::Connect { uri, login });
                }
            }
            0xff08 => {
                let buf = match f.field {
                    0 => &mut f.uri,
                    1 => &mut f.user,
                    _ => &mut f.pass,
                };
                if ctrl {
                    buf.clear();
                } else {
                    buf.pop();
                }
            }
            _ => {
                if let Some(c) = keysym_char(sym) {
                    if !c.is_control() {
                        match f.field {
                            0 => f.uri.push(c),
                            1 => f.user.push(c),
                            _ => f.pass.push(c),
                        }
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn draw_connect(&mut self) {
        let Some(f) = self.connect.as_ref() else {
            return;
        };
        let p = self.p;
        let (font, small, pad) = (self.m.font, self.m.small, self.m.pad);
        let (uri, user, pass, field, login, note) = (
            f.uri.clone(),
            f.user.clone(),
            f.pass.clone(),
            f.field,
            f.login,
            f.note.clone(),
        );
        let w = (self.cv.w * 2 / 3)
            .clamp((380.0 * self.m.s) as i32, (640.0 * self.m.s) as i32)
            .min(self.cv.w - 2 * pad);
        let rows = if login { 3 } else { 1 };
        let field_h = font.height + pad;
        let h = 4 * pad
            + font.height
            + rows * (field_h + small.height + pad)
            + 2 * small.height
            + 2 * pad;
        let r = Rect::new((self.cv.w - w) / 2, (self.cv.h - h) / 3, w, h);
        self.cv
            .tint(Rect::new(0, 0, self.cv.w, self.cv.h), p.background, 140);
        let sh = (4.0 * self.m.s) as i32;
        self.cv
            .tint(Rect::new(r.x + sh, r.y + sh, r.w, r.h), 0x000000, 120);
        self.cv.fill(r, p.surface);
        self.cv.frame(r, p.accent);
        let mut y = r.y + 2 * pad;
        crate::logo::word(
            &mut self.cv,
            r.x + 2 * pad,
            y,
            "CONNECT",
            (2.0 * self.m.s).round().max(2.0) as i32,
            p.accent,
            None,
        );
        y += font.height + pad;
        let labels = ["Address", "User", "Password"];
        for (k, label) in labels.iter().enumerate().take(rows as usize) {
            self.text
                .draw(&mut self.cv, r.x + 2 * pad, y, label, small, p.muted);
            y += small.height + pad / 3;
            let fr = Rect::new(r.x + 2 * pad, y, r.w - 4 * pad, field_h);
            self.cv.fill(fr, p.background);
            self.cv
                .frame(fr, if k == field { p.accent } else { p.border });
            let value = match k {
                0 => uri.clone(),
                1 => user.clone(),
                _ => "•".repeat(pass.chars().count()),
            };
            let shown = if k == field {
                format!("{value}▏")
            } else {
                value
            };
            let shown = self.text.ellipsize(&shown, font, fr.w - 2 * pad);
            self.text.draw(
                &mut self.cv,
                fr.x + pad,
                fr.y + (fr.h - font.height) / 2,
                &shown,
                font,
                p.foreground,
            );
            y += field_h + pad;
        }
        let hint =
            note.unwrap_or_else(|| "sftp://host  ftp://host  smb://host/share  davs://host".into());
        let hint = self.text.ellipsize(&hint, small, r.w - 4 * pad);
        self.text.draw(
            &mut self.cv,
            r.x + 2 * pad,
            y,
            &hint,
            small,
            if login { p.warning } else { p.muted },
        );
        y += small.height + pad / 2;
        let keys = if login {
            "Enter connect · Tab next field · Esc cancel"
        } else {
            "Enter connect · Tab completes an SSH host · Esc cancel"
        };
        self.text
            .draw(&mut self.cv, r.x + 2 * pad, y, keys, small, p.muted);
    }
}

/// Icon sizes of the zoom steps (Ctrl+wheel, Ctrl+plus/minus).
const ZOOM: [f32; 6] = [32.0, 48.0, 64.0, 96.0, 128.0, 192.0];
const ZOOM_DEFAULT: usize = 2;

fn metrics(text: &Text, font_px: f32, s: f32, zoom: usize) -> Metrics {
    let font = text.size(font_px);
    let small = text.size((font_px * 0.86).round());
    let pad = (8.0 * s).round() as i32;
    let icon = (ZOOM[zoom.min(ZOOM.len() - 1)] * s).round() as i32;
    let small_icon = ((22.0 * s).round() as i32).max(16);
    Metrics {
        s,
        font,
        small,
        pad,
        bar_h: font.height + 2 * pad + (6.0 * s) as i32,
        tab_h: font.height + pad + (6.0 * s) as i32,
        status_h: small.height + pad + (4.0 * s) as i32,
        side_w: (200.0 * s) as i32,
        icon,
        small_icon,
        cell_w: ((icon as f32 * 1.45).round() as i32).max((112.0 * s) as i32),
        cell_h: icon + 2 * font.height + 2 * pad + pad / 2,
        row_h: font.height.max(small_icon) + pad,
    }
}

/// Papirus has named icons for the XDG folders.
fn folder_icon(p: &Path) -> &'static str {
    let home = theme::home();
    if p == home {
        return "user-home";
    }
    if p.parent() == Some(home.as_path()) {
        match p.file_name().and_then(|n| n.to_str()) {
            Some("Desktop") => return "user-desktop",
            Some("Documents") => return "folder-documents",
            Some("Downloads") => return "folder-download",
            Some("Music") => return "folder-music",
            Some("Pictures") => return "folder-pictures",
            Some("Videos") => return "folder-videos",
            Some("Templates") => return "folder-templates",
            Some("Public") => return "folder-publicshare",
            Some("Nextcloud") => return "folder-nextcloud",
            Some("Git") | Some("Projects") => return "folder-git",
            _ => {}
        }
    }
    "folder"
}

fn expand(s: &str) -> String {
    if s == "~" {
        return theme::home().display().to_string();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return theme::home().join(rest).display().to_string();
    }
    s.to_string()
}

/// Latin-1 and Unicode keysyms to characters.
fn keysym_char(sym: u32) -> Option<char> {
    match sym {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(sym),
        0x0100_0000..=0x0110_ffff => char::from_u32(sym - 0x0100_0000),
        _ => None,
    }
}

/// Start a program detached from this one (its own session, no stdio).
fn spawn(cmd: &str, args: &[&std::ffi::OsStr], cwd: Option<&Path>) {
    use std::os::unix::process::CommandExt;
    let mut c = std::process::Command::new(cmd);
    c.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if let Ok(mut child) = c.spawn() {
        // Reap it in the background so no zombie is left.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms_map_to_swedish_letters_and_unicode() {
        assert_eq!(keysym_char(0x61), Some('a'));
        assert_eq!(keysym_char(0xe5), Some('å'));
        assert_eq!(keysym_char(0x0100_20ac), Some('€'));
        assert_eq!(keysym_char(0xff0d), None);
    }

    #[test]
    fn tilde_paths_expand_to_home() {
        let h = theme::home();
        assert_eq!(expand("~"), h.display().to_string());
        assert_eq!(expand("~/x"), h.join("x").display().to_string());
        assert_eq!(expand("/etc"), "/etc");
    }
}
