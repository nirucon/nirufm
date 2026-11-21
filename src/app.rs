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
}

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

impl App {
    pub fn new(start: PathBuf, started: Instant) -> Res<Self> {
        let base = theme::config_home();
        let font_px = theme::configured_font_size(&base).unwrap_or(15.0);
        let text = Text::new(theme::configured_font_path(&base).as_deref())?;
        let s = (font_px / 15.0).clamp(0.6, 3.0);
        let m = metrics(&text, font_px, s);
        let x = X::open((1120.0 * s) as u16, (720.0 * s) as u16, "NIRUFM")?;
        let theme_name = x
            .root_theme()
            .unwrap_or_else(|| theme::configured_theme(&base));
        let p = theme::load(&base, &theme_name);
        let _ = x.set_icon(&crate::logo::window_icons(&p));
        let inotify = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
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
        let mut any = false;
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
            any = true;
        }
        if any {
            // Many events arrive for one copy: reload once, a moment later.
            self.reload_at
                .get_or_insert(Instant::now() + Duration::from_millis(120));
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
            let mut fds = [
                libc::pollfd {
                    fd: xfd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.inotify,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let n = if self.inotify >= 0 { 2 } else { 1 };
            unsafe { libc::poll(fds.as_mut_ptr(), n, timeout) };
            if n == 2 && fds[1].revents & libc::POLLIN != 0 {
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
        let step = self.m.row_h * 3;
        match b {
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
                if let Some(p) = self.places.get(i).map(|p| p.path.clone()) {
                    self.go(p);
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
                // Escape
                self.typed.clear();
                self.clear_selection();
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
            0x71 => self.quit = true, // q
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
            0x6e if shift => {
                // Ctrl+Shift+N: new folder (prototype: named "New folder")
                let mut p = self.tab().path.join("New folder");
                let mut k = 2;
                while p.exists() {
                    p = self.tab().path.join(format!("New folder {k}"));
                    k += 1;
                }
                match std::fs::create_dir(&p) {
                    Ok(()) => self.flash(format!(
                        "Created {}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    )),
                    Err(e) => self.flash(format!("Could not create the folder: {e}")),
                }
            }
            _ => match sym {
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
        let mut y = m.bar_h + m.pad / 2;
        let row = (m.font.height + m.pad).max(m.small_icon + m.pad / 2);
        let cur = self.tab().path.clone();
        let mut devices_header = false;
        let (font, small, pad, si) = (m.font, m.small, m.pad, m.small_icon);
        for (i, pl) in self.places.clone().iter().enumerate() {
            if pl.device && !devices_header {
                devices_header = true;
                y += pad / 2;
                self.text
                    .draw(&mut self.cv, pad * 2, y, "DEVICES", small, p.muted);
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
            let label = self.text.ellipsize(&pl.label, font, sw - tx - pad);
            let col = if cur == pl.path {
                p.foreground
            } else {
                blend(p.muted, p.foreground, 0.45)
            };
            self.text.draw(
                &mut self.cv,
                tx,
                y + (row - font.height) / 2,
                &label,
                font,
                col,
            );
            self.hits.push((r, Hit::Place(i)));
            y += row;
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
            crate::logo::splash(&mut self.cv, c, &p, "NO ACCESS");
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
            if let Some(img) = self.icon_for(&e, icon) {
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
        self.cv.fill(Rect::new(track.x, ty, w, th), blend(self.p.border, self.p.muted, 0.5));
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
        if let Some(m) = msg {
            self.text
                .draw(&mut self.cv, r.x + pad, ty, &m, small, p.accent);
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

fn metrics(text: &Text, font_px: f32, s: f32) -> Metrics {
    let font = text.size(font_px);
    let small = text.size((font_px * 0.86).round());
    let pad = (8.0 * s).round() as i32;
    let icon = if s >= 1.6 {
        96
    } else if s >= 1.2 {
        64
    } else {
        48
    };
    let small_icon = if s >= 1.6 {
        32
    } else if s >= 1.2 {
        24
    } else {
        22
    };
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
        cell_w: (icon as f32 * 2.4).round() as i32,
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
