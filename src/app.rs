//! The file manager: tabs, the side panel, the path bar, the icon and list
//! views, and all keyboard and mouse handling. Drawing happens only when
//! something changed; between events the process sleeps in `poll`.
use crate::canvas::{Canvas, Rect};
use crate::dir::{self, Entry, SortKey};
use crate::edit::Key as EditKey;
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
use x11rb::wrapper::ConnectionExt as _;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Icons,
    Thumbs,
    Compact,
    List,
}

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Icons => "icons",
            View::Thumbs => "thumbnails",
            View::Compact => "compact",
            View::List => "list",
        }
    }
    pub fn from_name(s: &str) -> Option<View> {
        Some(match s {
            "icons" => View::Icons,
            "thumbnails" => View::Thumbs,
            "compact" => View::Compact,
            "list" => View::List,
            _ => return None,
        })
    }
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
    /// A search below `path` replaces the folder's own items.
    search: Option<String>,
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
            search: None,
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
    View(View),
    Content,
    MenuItem(usize),
    Pane(usize),
    Eject(usize),
    MenuOutside,
    /// A button of the dialog for a name that exists.
    Answer(crate::ops::Choice),
    AnswerAll,
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
    /// The path bar while a path is typed (Ctrl+L).
    path_edit: Option<crate::edit::LineEdit>,
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
    /// The jobs sent to the worker, in its order: why each was sent and
    /// its cancel flag. The first is the one running.
    jobs: std::collections::VecDeque<(Origin, std::sync::Arc<std::sync::atomic::AtomicBool>)>,
    /// A name that exists where a copy or move goes: the worker waits for
    /// the answer, the window does not.
    ask: Option<Ask>,
    undo: Vec<crate::ops::Undo>,
    clip: Option<(Vec<PathBuf>, bool)>,
    menu: Option<Menu>,
    confirm: Option<Confirm>,
    rename: Option<Rename>,
    /// Several items renamed at once.
    batch: Option<BatchRename>,
    /// Text copied from a text field; served on the clipboard when no
    /// files are.
    clip_text: Option<String>,
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
    /// A bookmark pressed in the side panel: its row and the press height.
    place_press: Option<(usize, i32)>,
    /// While a bookmark is dragged: the row it would move to.
    place_drag_to: Option<usize>,
    /// A bookmark being renamed in the side panel.
    place_rename: Option<(usize, crate::edit::LineEdit)>,
    side_scroll: i32,
    side_total: i32,
    apps: Option<crate::apps::Apps>,
    menu_apps: Vec<crate::apps::App>,
    /// The templates the open menu lists.
    menu_templates: Vec<PathBuf>,
    press: Option<(i32, i32)>,
    single_on_release: Option<usize>,
    drag: Option<DragOut>,
    drag_in: Option<DragIn>,
    drop_target: Option<DropAt>,
    dropped_files: Option<Vec<PathBuf>>,
    /// A rubber band being drawn over the files.
    band: Option<crate::band::Band>,
    /// Pixels per step the folder scrolls while a band or a drag is held at
    /// its edge, and when the next step is due.
    edge: i32,
    edge_next: Option<Instant>,
    /// The pointer during a band or a drag over this window.
    pointer: (i32, i32),
    /// A folder held under a drag opens after a moment.
    spring: Option<(DropAt, Instant)>,
    /// The label that follows the pointer while files are dragged, and what
    /// it says (it is redrawn only when that changes).
    label_win: Option<u32>,
    label_text: String,
    /// Remembered view per folder: view, zoom, sort, reverse.
    folders: std::collections::HashMap<PathBuf, (View, usize, SortKey, bool)>,
    gallery: Option<Gallery>,
    search_edit: Option<crate::edit::LineEdit>,
    /// The text fields drawn in the last frame: which, where (the visible
    /// part), the x where their text starts, for presses in them, and how
    /// far the text is scrolled, kept so it does not jump between frames.
    fields: Vec<(FieldId, Rect, i32, i32)>,
    fields_prev: Vec<(FieldId, Rect, i32, i32)>,
    /// The last press in a text field and how many came in quick succession.
    field_clicks: Option<(Instant, u8, FieldId)>,
    /// The button is held after a press in this field: drag to select.
    field_drag: Option<FieldId>,
    /// Two panes side by side: the tabs shown left and right.
    split: Option<(usize, usize)>,
    share_hosts: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    menu_hosts: Vec<String>,
    help: bool,
    job_started: Option<Instant>,
    nc_status: std::collections::HashMap<PathBuf, u8>,
    nc_tx: std::sync::mpsc::Sender<(PathBuf, u8)>,
    nc_rx: std::sync::mpsc::Receiver<(PathBuf, u8)>,
    props: Option<Props>,
    search_due: Option<Instant>,
    search_gen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    searching: bool,
    search_tx: std::sync::mpsc::Sender<(usize, Vec<Entry>, bool)>,
    search_rx: std::sync::mpsc::Receiver<(usize, Vec<Entry>, bool)>,
    gallery_tx: std::sync::mpsc::Sender<(PathBuf, Option<crate::canvas::Image>)>,
    gallery_rx: std::sync::mpsc::Receiver<(PathBuf, Option<crate::canvas::Image>)>,
}

/// The photo and film viewer.
struct Gallery {
    items: Vec<Entry>,
    pos: usize,
    images: std::collections::HashMap<PathBuf, Option<std::rc::Rc<crate::canvas::Image>>>,
    fitted: Option<(PathBuf, u8, i32, i32, std::rc::Rc<crate::canvas::Image>)>,
    strip: std::collections::HashMap<PathBuf, Option<std::rc::Rc<crate::canvas::Image>>>,
    exif: std::collections::HashMap<PathBuf, Option<crate::exif::Exif>>,
    slideshow: Option<Instant>,
    zoom: f32,
    pan: (i32, i32),
    rot: u8,
    info: bool,
    full: bool,
    grab: Option<(i32, i32, i32, i32)>,
    last_click: Option<Instant>,
}

const SLIDE: Duration = Duration::from_secs(4);
/// How often the folder scrolls while a band or drag is held at its edge.
const EDGE_STEP: Duration = Duration::from_millis(25);
/// How long a folder is held under a drag before it opens.
const SPRING: Duration = Duration::from_millis(800);

/// Files being dragged out of this window.
struct DragOut {
    files: Vec<PathBuf>,
    target: Option<(Window, u32)>,
    accepted: bool,
    finished_wait: Option<Instant>,
}

/// Another program dragging over this window.
struct DragIn {
    source: Window,
    version: u32,
    files_offered: bool,
    move_offered: bool,
}

/// Where a drop would land: a folder item, a place, a tab or the folder shown.
#[derive(Clone, Debug, PartialEq)]
enum DropAt {
    Item(usize),
    Place(usize),
    Tab(usize),
    Here,
}

/// An item's name edited in place.
struct Rename {
    /// The item by its path, not its row: a reload while typing (another
    /// program writing in the folder) can move it in the list.
    path: PathBuf,
    ed: crate::edit::LineEdit,
}

/// The dialog for renaming several items: find and replace in the names
/// before their extensions, with a running number.
struct BatchRename {
    paths: Vec<PathBuf>,
    /// (name, is a folder) of each path.
    items: Vec<(String, bool)>,
    find: crate::edit::LineEdit,
    replace: crate::edit::LineEdit,
    /// 0 find, 1 replace.
    field: usize,
    note: Option<String>,
}

/// A text field the mouse can work in, by what it edits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FieldId {
    Rename,
    Bookmark,
    Path,
    Search,
    /// 0 address, 1 user, 2 password.
    Connect(usize),
    /// 0 find, 1 replace.
    Batch(usize),
}

struct ConnectForm {
    uri: crate::edit::LineEdit,
    user: crate::edit::LineEdit,
    pass: crate::edit::LineEdit,
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

/// The properties sheet: totals are counted on a thread.
struct Props {
    paths: Vec<PathBuf>,
    /// bytes, files, folders, done
    totals: std::sync::Arc<std::sync::Mutex<(u64, u64, u64, bool)>>,
    sha: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

/// Why a job was sent, which decides where its undo step goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Origin {
    New,
    Undo,
}

/// The question for a name that exists, from the worker.
struct Ask {
    name: String,
    folder: PathBuf,
    existing: crate::ops::Side,
    incoming: crate::ops::Side,
    /// "Apply to all remaining" is ticked.
    all: bool,
}

struct Running {
    verb: &'static str,
    done: u64,
    total: u64,
    current: String,
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
    NewFile,
    /// A copy of the template at this place in `menu_templates`.
    NewFrom(u8),
    Terminal,
    Hidden,
    CopyPath,
    Bookmark,
    RemoveBookmark,
    BookmarkUp,
    BookmarkDown,
    RenameBookmark,
    PlaceTab,
    BookmarkSelected,
    Connect,
    NcPublic,
    NcInternal,
    NcShare,
    NcBrowser,
    Props,
    OpenWith(u8),
    Wallpaper,
    SendTo(u8),
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

/// The search text that marks the Recent view.
const RECENT: &str = "recent files";

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
        let (gallery_tx, gallery_rx) = std::sync::mpsc::channel();
        let (search_tx, search_rx) = std::sync::mpsc::channel();
        let (nc_tx, nc_rx) = std::sync::mpsc::channel();
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
            icons: Icons::new(&p, crate::icons::configured_style(&theme::config_home())),
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
            jobs: std::collections::VecDeque::new(),
            ask: None,
            pending: 0,
            undo: Vec::new(),
            clip: None,
            menu: None,
            confirm: None,
            rename: None,
            batch: None,
            clip_text: None,
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
            place_press: None,
            place_drag_to: None,
            place_rename: None,
            side_scroll: 0,
            side_total: 0,
            apps: None,
            menu_apps: Vec::new(),
            menu_templates: Vec::new(),
            press: None,
            single_on_release: None,
            drag: None,
            drag_in: None,
            drop_target: None,
            dropped_files: None,
            band: None,
            edge: 0,
            edge_next: None,
            pointer: (0, 0),
            spring: None,
            label_win: None,
            label_text: String::new(),
            folders: load_folders(),
            gallery: None,
            search_edit: None,
            fields: Vec::new(),
            fields_prev: Vec::new(),
            field_clicks: None,
            field_drag: None,
            split: None,
            share_hosts: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            menu_hosts: Vec::new(),
            help: false,
            job_started: None,
            nc_status: std::collections::HashMap::new(),
            nc_tx,
            nc_rx,
            props: None,
            search_due: None,
            search_gen: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            searching: false,
            search_tx,
            search_rx,
            gallery_tx,
            gallery_rx,
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
        if reset {
            self.tabs[i].search = None;
        } else if self.tabs[i].search.is_some() {
            // Search results stay; only the hidden filter is applied again.
            let hidden = self.hidden;
            let t = &mut self.tabs[i];
            t.shown = (0..t.all.len())
                .filter(|k| hidden || !t.all[*k].hidden())
                .collect();
            t.selected = vec![false; t.shown.len()];
            t.cursor = t.cursor.min(t.shown.len().saturating_sub(1));
            self.dirty = true;
            return;
        }
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
            if i == self.active {
                self.apply_memory(&path);
            }
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
            if reset {
                self.nextcloud_status();
            }
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
            // The same folder, but showing search results or Recent: show
            // the folder itself again.
            if self.tab().search.is_some() {
                self.search_edit = None;
                self.search_gen
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.searching = false;
                let a = self.active;
                self.load_tab(a, true);
            }
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
        self.split = None;
        if self.active >= self.tabs.len() || self.active > i {
            self.active = self.active.saturating_sub(1);
        }
        let a = self.active;
        self.load_tab(a, false);
    }

    fn switch(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            // In two panes, a tab shown nowhere takes the active pane.
            if let Some((l, r)) = self.split {
                if i != l && i != r {
                    self.split = Some(if self.active == l { (i, r) } else { (l, i) });
                }
            }
            self.active = i;
            let p = self.tabs[i].path.clone();
            self.apply_memory(&p);
            let a = self.active;
            self.load_tab(a, false);
        }
    }

    fn open(&mut self, i: usize) {
        let Some(e) = self.tab().entry(i).cloned() else {
            return;
        };
        if e.mime.starts_with("image/") && e.mime != "image/svg+xml"
            || e.mime == "image/svg+xml" && e.size < 4 << 20
        {
            self.open_gallery(Some(e.path.clone()));
            return;
        }
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
            self.icons.set_theme(&p);
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

    /// Cell width, cell height and picture size of the grid views.
    fn grid(&self) -> (i32, i32, i32) {
        let m = &self.m;
        match self.view {
            View::Thumbs => {
                let b = self.thumb_box();
                (b + 2 * m.pad, b + m.font.height + 2 * m.pad + m.pad / 2, b)
            }
            View::Compact => ((240.0 * m.s) as i32, m.row_h, m.small_icon),
            _ => (m.cell_w, m.cell_h, m.icon),
        }
    }

    /// The picture size of the thumbnail view (follows the zoom).
    fn thumb_box(&self) -> i32 {
        ((self.m.icon as f32 * 2.2) as i32)
            .clamp((96.0 * self.m.s) as i32, (360.0 * self.m.s) as i32)
    }

    /// The size thumbnails are drawn at in the current view.
    fn picture_size(&self) -> i32 {
        match self.view {
            View::Thumbs => self.thumb_box(),
            View::Compact | View::List => 0,
            View::Icons => self.m.icon,
        }
    }

    fn item_rect(&self, i: usize) -> Rect {
        let c = self.content;
        match self.view {
            View::List => Rect::new(
                c.x,
                c.y + self.m.row_h + i as i32 * self.m.row_h - self.tab().scroll,
                c.w,
                self.m.row_h,
            ),
            _ => {
                let (cw, ch, _) = self.grid();
                let cols = self.cols.max(1);
                let (col, row) = (i as i32 % cols, i as i32 / cols);
                Rect::new(
                    c.x + self.m.pad + col * cw,
                    c.y + self.m.pad + row * ch - self.tab().scroll,
                    cw,
                    ch,
                )
            }
        }
    }

    fn content_height(&self) -> i32 {
        let n = self.tab().shown.len() as i32;
        match self.view {
            View::List => (n + 1) * self.m.row_h,
            _ => {
                let (_, ch, _) = self.grid();
                let rows = (n + self.cols.max(1) - 1) / self.cols.max(1);
                rows * ch + 2 * self.m.pad
            }
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
            View::List => (self.content.h / self.m.row_h - 1).max(1) as usize,
            _ => ((self.content.h / self.grid().1).max(1) * self.cols.max(1)) as usize,
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
            if self.search_due.is_some_and(|t| Instant::now() >= t) {
                self.search_due = None;
                let q = self
                    .search_edit
                    .as_ref()
                    .map(|e| e.text.clone())
                    .unwrap_or_default();
                self.start_search(q);
            }
            if let Some(g) = &self.gallery {
                if g.slideshow.is_some_and(|t| t.elapsed() >= SLIDE) {
                    self.gallery_step(1, true);
                }
            }
            if self.edge != 0 {
                self.edge_step();
            }
            if self.spring.is_some() {
                self.spring_open();
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
                // For the end-to-end test: where the side panel rows are.
                if std::env::var_os("NIRUFM_PLACES").is_some() {
                    for (r, h) in &self.hits {
                        match h {
                            Hit::Place(i) => {
                                if let Some(pl) = self.places.get(*i) {
                                    eprintln!(
                                        "place {} {} {}",
                                        pl.label,
                                        r.x + r.w / 2,
                                        r.y + r.h / 2
                                    );
                                }
                            }
                            Hit::Item(i) => {
                                if let Some(e) = self.tab().entry(*i) {
                                    eprintln!("item {} {} {} {} {}", e.label, r.x, r.y, r.w, r.h);
                                }
                            }
                            Hit::Answer(c) => {
                                eprintln!("answer {c:?} {} {}", r.x + r.w / 2, r.y + r.h / 2)
                            }
                            _ => {}
                        }
                    }
                    let fields: Vec<(FieldId, Rect)> =
                        self.fields.iter().map(|f| (f.0, f.1)).collect();
                    for (id, r) in fields {
                        if let Some(ed) = self.field_edit(id) {
                            eprintln!(
                                "field {id:?} {} {} {} {} cursor {} anchor {} text {}",
                                r.x, r.y, r.w, r.h, ed.cursor, ed.anchor, ed.text
                            );
                        }
                    }
                    if let Some(a) = &self.ask {
                        eprintln!("ask {} all {}", a.name, a.all);
                    }
                    let t = self.tab();
                    eprintln!(
                        "state {} selected {} dir {}",
                        t.selected.iter().filter(|s| **s).count(),
                        t.shown.len(),
                        t.path.display()
                    );
                }
                continue;
            }
            self.x.c.flush()?;
            let mut timeout: i32 = -1;
            if let Some(at) = self.reload_at {
                timeout = at.saturating_duration_since(Instant::now()).as_millis() as i32 + 1;
            }
            if let Some(t) = self.search_due {
                let left = t.saturating_duration_since(Instant::now()).as_millis() as i32 + 1;
                timeout = if timeout < 0 { left } else { timeout.min(left) };
            }
            if let Some(t) = self.gallery.as_ref().and_then(|g| g.slideshow) {
                let left = SLIDE.saturating_sub(t.elapsed()).as_millis() as i32 + 1;
                timeout = if timeout < 0 { left } else { timeout.min(left) };
            }
            if self.edge != 0 {
                let left = self.edge_next.map_or(0, |t| {
                    t.saturating_duration_since(Instant::now()).as_millis() as i32
                }) + 1;
                timeout = if timeout < 0 { left } else { timeout.min(left) };
            }
            if let Some((_, t)) = &self.spring {
                let left = t.saturating_duration_since(Instant::now()).as_millis() as i32 + 1;
                timeout = if timeout < 0 { left } else { timeout.min(left) };
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
            Event::ClientMessage(e) if self.is_xdnd(e.type_) => {
                self.xdnd_message(e.type_, e.data.as_data32())
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
                if e.selection == self.x.atoms.xdnd_selection {
                    if let Some(d) = &self.drag {
                        let _ = self.x.answer(&e, &d.files.clone(), false);
                    } else if let Some(files) = self.dropped_files.clone() {
                        let _ = self.x.answer(&e, &files, false);
                    }
                } else if let Some((files, cut)) = self.clip.clone() {
                    let _ = self.x.answer(&e, &files, cut);
                } else if let Some(text) = &self.clip_text {
                    let _ = self.x.answer_text(&e, text);
                }
            }
            Event::SelectionClear(_) => {}
            Event::SelectionNotify(e) if e.selection == self.x.atoms.xdnd_selection => {
                self.drop_data(e.property)
            }
            Event::SelectionNotify(e) => self.pasted(e.target, e.property),
            Event::ButtonRelease(_) if self.gallery.is_some() => {
                if let Some(g) = self.gallery.as_mut() {
                    g.grab = None;
                }
            }
            Event::MotionNotify(e) if self.gallery.as_ref().is_some_and(|g| g.grab.is_some()) => {
                if let Some(g) = self.gallery.as_mut() {
                    if let Some((sx, sy, px, py)) = g.grab {
                        g.pan = (px + e.event_x as i32 - sx, py + e.event_y as i32 - sy);
                        self.dirty = true;
                    }
                }
            }
            Event::ButtonRelease(e) if e.detail == 1 && self.place_press.is_some() => {
                self.place_release()
            }
            Event::MotionNotify(e) if self.place_press.is_some() => {
                self.place_motion(e.event_x as i32, e.event_y as i32)
            }
            Event::ButtonRelease(e) if e.detail == 1 && self.field_drag.is_some() => {
                self.field_drag = None
            }
            Event::MotionNotify(e) if self.field_drag.is_some() => {
                self.field_drag_to(e.event_x as i32)
            }
            Event::ButtonRelease(e) if e.detail == 1 && self.band.is_some() => self.band_release(),
            Event::MotionNotify(e) if self.band.is_some() => {
                self.band_motion(e.event_x as i32, e.event_y as i32)
            }
            Event::ButtonRelease(e) if e.detail == 1 => self.release(
                e.event_x as i32,
                e.event_y as i32,
                e.root_x,
                e.root_y,
                e.state,
            ),
            Event::MotionNotify(e)
                if self.drag.is_some()
                    || (e.state.contains(KeyButMask::BUTTON1) && self.press.is_some()) =>
            {
                self.drag_motion(
                    e.event_x as i32,
                    e.event_y as i32,
                    e.root_x,
                    e.root_y,
                    e.state,
                )
            }
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
        if self.ask.is_some() {
            match (b, self.hit(x, y)) {
                (1, Some(Hit::Answer(c))) => self.answer(c),
                (1, Some(Hit::AnswerAll)) => {
                    if let Some(a) = self.ask.as_mut() {
                        a.all = !a.all;
                    }
                    self.dirty = true;
                }
                _ => {}
            }
            return;
        }
        if self.gallery.is_some() {
            return self.gallery_button(b, x, y);
        }
        if self.confirm.is_some() {
            return;
        }
        if self.preview.is_some() {
            self.preview = None;
            self.dirty = true;
            return;
        }
        if b == 1 && self.menu.is_none() {
            if let Some(id) = self.field_at(x, y) {
                return self.field_press(id, x, shift);
            }
        }
        if self.connect.is_some() {
            // The form is modal: only its fields take presses.
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
        if self.batch.is_some() {
            return;
        }
        if self.rename.is_some() && matches!(b, 1..=3) {
            // A click elsewhere applies the name, as in other file managers.
            self.commit_rename(false);
        }
        if self.place_rename.is_some() {
            self.commit_place_rename();
        }
        if b == 3 {
            match self.hit(x, y) {
                Some(Hit::Place(i)) if self.places.get(i).is_some_and(|p| p.bookmark) => {
                    self.menu_place = Some(i);
                    self.menu = Some(Menu {
                        x,
                        y,
                        items: vec![
                            ("Open in a new tab".into(), Act::PlaceTab, ""),
                            ("Move up".into(), Act::BookmarkUp, ""),
                            ("Move down".into(), Act::BookmarkDown, ""),
                            ("Rename bookmark".into(), Act::RenameBookmark, ""),
                            ("Remove bookmark".into(), Act::RemoveBookmark, ""),
                        ],
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
        let Some(mut h) = self.hit(x, y) else { return };
        // Beside a file's picture and name is room to start a band.
        if let Hit::Item(i) = h {
            if b == 1 && !self.item_body(i).contains(x, y) {
                h = Hit::Content;
            }
        }
        match (b, h) {
            // A bookmark opens when the button is released, so it can be
            // dragged to another place in the list first.
            (1, Hit::Place(i)) if self.places.get(i).is_some_and(|p| p.bookmark) => {
                self.place_press = Some((i, y));
                self.place_drag_to = None;
            }
            (1, Hit::Place(i)) => self.open_place(i),
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
            (1 | 3, Hit::Pane(k)) => self.focus_pane(k),
            (1, Hit::Back) => self.back(),
            (1, Hit::View(v)) => self.set_view(v),
            (1, Hit::Forward) => self.forward(),
            (1, Hit::Up) => self.up(),
            (1, Hit::Header(k)) => {
                if self.sort == k {
                    self.reverse = !self.reverse;
                } else {
                    self.sort = k;
                    self.reverse = false;
                }
                self.remember_folder();
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
                    let already = self.tab().selected.get(i).copied().unwrap_or(false);
                    // A press on a selection may start a drag of all of it;
                    // the selection becomes just this item only on release.
                    if already && !ctrl && !shift {
                        self.tab_mut().cursor = i;
                        self.single_on_release = Some(i);
                        self.dirty = true;
                    } else {
                        self.set_cursor(i, shift, ctrl);
                    }
                    self.press = Some((x, y));
                }
            }
            (2, Hit::Item(i)) => {
                if let Some(e) = self.tab().entry(i).cloned() {
                    if e.is_dir {
                        self.new_tab(e.path);
                    }
                }
            }
            (1, Hit::Content) => self.start_band(x, y, ctrl, shift),
            _ => {}
        }
    }

    fn key(&mut self, code: u8, state: KeyButMask) {
        let sym = self.x.keysym(code, state);
        let ctrl = state.contains(KeyButMask::CONTROL);
        let shift = state.contains(KeyButMask::SHIFT);
        let alt = state.contains(KeyButMask::MOD1);
        if self.ask.is_some() {
            return self.ask_key(sym);
        }
        if self.gallery.is_some() {
            return self.gallery_key(sym, ctrl);
        }
        if sym == 0xff1b && self.band.is_some() {
            return self.band_cancel();
        }
        if sym == 0xff1b && self.drag.is_some() {
            return self.drag_cancel();
        }
        if self.help && sym != 0xffbe {
            self.help = false;
            self.dirty = true;
            return;
        }
        if self.props.is_some() {
            match sym {
                0x73 => self.props_checksum(), // s
                _ => self.props = None,
            }
            self.dirty = true;
            return;
        }
        if self.connect.is_some() {
            return self.connect_key(sym, ctrl, shift);
        }
        if self.batch.is_some() {
            return self.batch_key(sym, ctrl, shift);
        }
        if self.search_edit.is_some() {
            return self.search_key(sym, ctrl, shift);
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
            return self.rename_key(sym, ctrl, shift);
        }
        if self.place_rename.is_some() {
            return self.place_rename_key(sym, ctrl, shift);
        }
        if self.preview.is_some() {
            return self.preview_key(sym);
        }
        if self.path_edit.is_some() {
            return self.path_key(sym, ctrl, shift);
        }
        let t = self.tab();
        let (cur, n) = (t.cursor, t.shown.len());
        let cols = if self.view != View::List {
            self.cols.max(1) as usize
        } else {
            1
        };
        // keysyms: X11/keysymdef.h
        match sym {
            0xff1b => {
                // Escape: cancel a running job, else clear the selection.
                if self.running.is_some() {
                    // The job running now, the first sent of those left.
                    if let Some((_, cancel)) = self.jobs.front() {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
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
            0xffc0 => self.toggle_split(),   // F3
            0xffbe => {
                // F1: the keys
                self.help = !self.help;
                self.dirty = true;
            }
            0xff09 if !ctrl && self.split.is_some() => {
                if let Some((other, _)) = self.other_pane() {
                    self.focus_pane(other);
                }
            }
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
            0x6e | 0x4e if ctrl && alt => self.act(Act::NewFile), // Ctrl+Alt+N
            0xff0d | 0xff8d => {
                if alt {
                    return self.open_props();
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
                open_terminal(&p);
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
            0x67 => self.open_gallery(None),       // g
            0x66 => {
                // f: search below this folder
                let q = self.tab().search.clone().unwrap_or_default();
                let q = if q == RECENT { String::new() } else { q };
                self.search_edit = Some(crate::edit::LineEdit::selecting(&q, 0, q.len()));
                self.dirty = true;
            }
            0x64 => self.act(Act::Bookmark), // d
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
                // All selected: typing replaces it, End or a click edits it.
                let path = places::pretty(&self.tab().path);
                self.path_edit = Some(crate::edit::LineEdit::selecting(&path, 0, path.len()));
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
                0x31 => self.set_view(View::Icons),   // 1
                0x32 => self.set_view(View::Thumbs),  // 2
                0x33 => self.set_view(View::Compact), // 3
                0x34 => self.set_view(View::List),    // 4
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
            self.thumbs.clear();
            self.thumb_img.clear();
            self.dirty = true;
            self.layout_only();
            self.reveal();
            self.remember_folder();
        }
    }

    fn path_key(&mut self, sym: u32, ctrl: bool, shift: bool) {
        let ch = keysym_char(sym);
        let Some(ed) = self.path_edit.as_mut() else {
            return;
        };
        self.dirty = true;
        match ed.key(sym, ctrl, shift, ch) {
            EditKey::Cancel => self.path_edit = None,
            EditKey::Next(false) => {
                // Tab: complete a folder name.
                if let Some(done) = complete_folder(&ed.text, self.hidden) {
                    ed.select_all();
                    ed.insert(&done);
                }
            }
            EditKey::Commit => {
                let target = PathBuf::from(expand(&ed.text));
                self.path_edit = None;
                if target.is_dir() {
                    self.go(target);
                } else if target.exists() {
                    spawn("xdg-open", &[target.as_os_str()], None);
                } else {
                    self.flash(format!("No such folder: {}", target.display()));
                }
            }
            EditKey::Copy => {
                let s = ed.selected().to_string();
                self.copy_text(s);
            }
            EditKey::Cut => {
                if let Some(s) = ed.cut() {
                    self.copy_text(s);
                }
            }
            EditKey::Paste => self.paste_text(),
            EditKey::Next(true) | EditKey::Edited | EditKey::Ignored => {}
        }
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
        if let Some((l, _)) = self.split {
            let half = self.content.w / 2;
            self.content = if self.active == l {
                Rect::new(self.content.x, self.content.y, half - 1, self.content.h)
            } else {
                Rect::new(
                    self.content.x + half + 1,
                    self.content.y,
                    self.content.w - half - 1,
                    self.content.h,
                )
            };
        }
        let pad = m.pad;
        let (cw, _, _) = self.grid();
        self.cols = ((self.content.w - 2 * pad) / cw).max(1);
    }

    /// The other pane: its tab and rectangle.
    fn other_pane(&self) -> Option<(usize, Rect)> {
        let (l, r) = self.split?;
        let other = if self.active == l { r } else { l };
        let c = self.content;
        let full_x = self.m.side_w + 1;
        let full_w = self.cv.w - self.m.side_w - 1;
        let half = full_w / 2;
        let rect = if other == l {
            Rect::new(full_x, c.y, half - 1, c.h)
        } else {
            Rect::new(full_x + half + 1, c.y, full_w - half - 1, c.h)
        };
        Some((other, rect))
    }

    fn toggle_split(&mut self) {
        if self.split.take().is_some() {
            self.dirty = true;
            return;
        }
        if self.tabs.len() == 1 {
            let p = self.tab().path.clone();
            self.tabs.push(Tab::new(p));
            let k = self.tabs.len() - 1;
            self.load_tab(k, true);
            self.split = Some((self.active, k));
        } else {
            let other = (self.active + 1) % self.tabs.len();
            self.split = Some((self.active.min(other), self.active.max(other)));
        }
        self.dirty = true;
    }

    fn focus_pane(&mut self, idx: usize) {
        if idx != self.active {
            self.active = idx;
            let p = self.tabs[idx].path.clone();
            self.apply_memory(&p);
            self.rewatch();
            self.dirty = true;
        }
    }

    fn draw(&mut self) {
        self.hits.clear();
        self.fields_prev = std::mem::take(&mut self.fields);
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
        if let Some((other, rect)) = self.other_pane() {
            // Draw the other pane as if it were active, then make it one
            // clickable area that focuses it.
            let (keep_active, keep_content, keep_cols) = (self.active, self.content, self.cols);
            let keep_hover = self.hover.take();
            self.active = other;
            self.content = rect;
            let (cw, _, _) = self.grid();
            self.cols = ((rect.w - 2 * self.m.pad) / cw).max(1);
            let mark = self.hits.len();
            self.draw_content();
            self.hits.truncate(mark);
            self.cv.tint(rect, self.p.background, 70);
            self.hits.push((rect, Hit::Pane(other)));
            self.active = keep_active;
            self.content = keep_content;
            self.cols = keep_cols;
            self.hover = keep_hover;
            let line = Rect::new(
                rect.x.max(keep_content.x) - 1
                    + if rect.x > keep_content.x {
                        0
                    } else {
                        rect.w + 1
                    },
                rect.y,
                1,
                rect.h,
            );
            self.cv.fill(line, self.p.border);
        }
        self.draw_content();
        if self.split.is_some() {
            let c = self.content;
            self.cv.fill(
                Rect::new(c.x, c.y, c.w, (2.0 * self.m.s).max(2.0) as i32),
                self.p.accent,
            );
        }
        self.draw_status();
        self.draw_rename();
        self.draw_menu();
        self.draw_drop_target();
        if self.gallery.is_some() {
            self.draw_gallery();
            self.draw_ask();
            return;
        }
        self.draw_preview();
        self.draw_props();
        self.draw_help();
        self.draw_confirm();
        self.draw_connect();
        self.draw_batch();
        self.draw_ask();
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
            // Recent shows files from everywhere: only its own row is marked.
            let recent = self.tab().search.as_deref() == Some(RECENT);
            let here = if recent {
                pl.icon == "document-open-recent"
            } else {
                pl.mounted && !pl.path.as_os_str().is_empty() && cur == pl.path
            };
            if here {
                self.cv.fill(r, p.selected);
                self.cv.fill(
                    Rect::new(r.x, r.y, (2.0 * self.m.s).max(2.0) as i32, r.h),
                    p.accent,
                );
            }
            let mut names = vec![pl.icon.to_string(), "folder".to_string()];
            if pl.icon == "user-trash" && trash_has_files(&pl.path) {
                names.insert(0, "user-trash-full".into());
            }
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
            // A bookmark being dragged: a line where it would land.
            if let (Some((from, _)), Some(to)) = (self.place_press, self.place_drag_to) {
                if to == i && from != i {
                    let lw = (2.0 * self.m.s).max(2.0) as i32;
                    let ly = if from > i { r.y } else { r.y + r.h - lw };
                    self.cv.fill(Rect::new(r.x, ly, r.w, lw), p.accent);
                }
            }
            if let Some((_, ed)) = self.place_rename.take_if(|(k, _)| *k == i) {
                let field = Rect::new(tx - pad / 2, y + 1, sw - tx - pad / 2, row - 2);
                self.cv.fill(field, p.background);
                self.cv.fill(
                    Rect::new(field.x, field.y + field.h - 1, field.w, 1),
                    p.accent,
                );
                self.draw_edit(FieldId::Bookmark, (field, field), &ed, true);
                self.place_rename = Some((i, ed));
                y += row;
                self.hits.push((r, Hit::Place(i)));
                continue;
            }
            let label = self.text.ellipsize(&pl.label, font, sw - tx - pad - ew);
            let col = if here {
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
        // View buttons at the right end, drawn as pixel glyphs.
        let bs = font.height + pad / 2;
        let mut bx = bar.x + bar.w - pad - 4 * (bs + pad / 2);
        for v in [View::Icons, View::Thumbs, View::Compact, View::List] {
            let r = Rect::new(bx, (bar_h - bs) / 2, bs, bs);
            let on = self.view == v;
            if on {
                self.cv.fill(r, p.selected);
                self.cv.frame(r, p.accent);
            }
            let col = if on { p.foreground } else { p.muted };
            view_glyph(&mut self.cv, r.inset((bs / 5).max(2)), v, col);
            self.hits.push((r, Hit::View(v)));
            bx += bs + pad / 2;
        }
        let bar = Rect::new(bar.x, bar.y, bar.w - 4 * (bs + pad / 2) - pad, bar.h);
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
        let editing = self.search_edit.is_some();
        let search_shown = self
            .search_edit
            .as_ref()
            .map(|e| e.text.clone())
            .or_else(|| self.tab().search.clone());
        if let Some(q) = search_shown {
            let r = Rect::new(x, pad / 2, bar.x + bar.w - x - pad, bar_h - pad);
            self.cv.fill(r, p.surface);
            self.cv.frame(r, if editing { p.accent } else { p.border });
            let recent = q == RECENT && !editing;
            let label = if recent {
                "Recently used files".to_string()
            } else {
                // A long path gives way, the field keeps at least half.
                let room = r.w / 2 - self.text.width("Search : ", font);
                let at = places::pretty(&self.tab().path);
                format!("Search {}: ", self.text.ellipsize(&at, font, room.max(0)))
            };
            let lw = self
                .text
                .draw(&mut self.cv, r.x + pad, ty, &label, font, p.muted);
            let fr = Rect::new(r.x + pad + lw - pad / 2, r.y, r.w - lw - pad, r.h);
            if let Some(ed) = self.search_edit.take() {
                self.draw_edit(FieldId::Search, (fr, fr.intersect(&r.inset(1))), &ed, true);
                self.search_edit = Some(ed);
            } else if !recent {
                let shown = self.text.ellipsize(&q, font, fr.w - pad);
                self.text
                    .draw(&mut self.cv, fr.x + pad / 2, ty, &shown, font, p.foreground);
            }
            return;
        }
        if let Some(ed) = self.path_edit.take() {
            let r = Rect::new(x, pad / 2, bar.x + bar.w - x - pad, bar_h - pad);
            self.cv.fill(r, p.surface);
            self.cv.frame(r, p.accent);
            let fr = Rect::new(r.x + pad / 2, r.y, r.w - pad, r.h);
            self.draw_edit(FieldId::Path, (fr, r.inset(1)), &ed, true);
            self.path_edit = Some(ed);
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
            View::List => self.draw_list(),
            _ => self.draw_icons(),
        }
        self.draw_band();
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
        let (pad, font) = (self.m.pad, self.m.font);
        let (cell_w, cell_h, icon) = self.grid();
        let compact = self.view == View::Compact;
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
            let inner = r.inset(if compact { 1 } else { (2.0 * self.m.s) as i32 });
            if sel {
                self.cv.fill(inner, p.selected);
            } else if hover == Some(i) {
                self.cv.fill(inner, blend(p.background, p.surface, 0.9));
            }
            if i == cursor {
                self.cv.frame(inner, if sel { p.accent } else { p.border });
            }
            let col = if e.hidden() { p.muted } else { p.foreground };
            if compact {
                if let Some(img) = self.icon_for(&e, icon) {
                    self.cv.image(
                        r.x + pad / 2,
                        r.y + (r.h - img.h) / 2,
                        &img,
                        if e.hidden() { 150 } else { 255 },
                    );
                }
                let tx = r.x + pad + icon;
                let name = self.text.ellipsize(&e.label, font, r.w - icon - 2 * pad);
                self.text.draw(
                    &mut self.cv,
                    tx,
                    r.y + (r.h - font.height) / 2,
                    &name,
                    font,
                    col,
                );
                self.hits.push((r.intersect(&c), Hit::Item(i)));
                continue;
            }
            let thumb = self.thumb_for(&e);
            if let Some(img) = thumb {
                // A thin frame makes photos read as pictures, not icons.
                let (tx, ty) = (
                    r.x + (r.w - img.w) / 2,
                    r.y + pad + (icon - img.h) / if self.view == View::Thumbs { 2 } else { 1 },
                );
                self.cv
                    .frame(Rect::new(tx - 1, ty - 1, img.w + 2, img.h + 2), p.border);
                self.cv.image(tx, ty, &img, 255);
                if e.mime.starts_with("video/") {
                    // A film strip edge marks a video.
                    let sw = (4.0 * self.m.s).max(3.0) as i32;
                    let mut yy = ty + 2;
                    while yy + sw < ty + img.h {
                        self.cv.fill(Rect::new(tx + 2, yy, sw, sw), p.background);
                        self.cv
                            .fill(Rect::new(tx + img.w - sw - 2, yy, sw, sw), p.background);
                        yy += sw * 2;
                    }
                }
            } else if let Some(img) = self.icon_for(&e, icon.min(256)) {
                let alpha = if e.hidden() { 150 } else { 255 };
                let iy = r.y + pad + (icon - img.h) / 2;
                self.cv.image(r.x + (r.w - img.w) / 2, iy, &img, alpha);
            }
            let u = (2.0 * self.m.s).max(2.0) as i32;
            self.draw_nc_badge(
                &e.path,
                r.x + (r.w + icon) / 2 - 5 * u,
                r.y + pad + icon - 6 * u,
            );
            let max_lines = if self.view == View::Thumbs { 1 } else { 2 };
            // The rename field stands in for the name; a second line of
            // the old one would show below it.
            let max_lines = if self.rename.as_ref().is_some_and(|r| r.path == e.path) {
                0
            } else {
                max_lines
            };
            let lines = self.text.wrap(&e.label, font, cell_w - pad, max_lines);
            let mut ty = r.y + pad + icon + pad / 2;
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
            self.draw_nc_badge(&e.path, c.x + pad + si - 4, r.y + row_h / 2);
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
        let mut left = if t.search.is_some() {
            format!(
                "{n} found{}",
                if self.searching {
                    " · searching…"
                } else {
                    ""
                }
            )
        } else {
            format!("{n} item{}", if n == 1 { "" } else { "s" })
        };
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
        } else if self.drag.is_some() || self.band.as_ref().is_some_and(|b| b.live) {
            let hint = if self.drag.is_some() {
                "Ctrl copies · Shift moves · hold over a folder to open it · Esc cancels"
                    .to_string()
            } else {
                format!("{nsel} selected · Ctrl adds · Shift flips · Esc undoes")
            };
            let hint = self.text.ellipsize(&hint, small, r.w * 3 / 4);
            self.text
                .draw(&mut self.cv, r.x + pad, ty, &hint, small, p.accent);
        } else if let Some(m) = msg {
            let col = if self.error { p.error } else { p.accent };
            self.text.draw(&mut self.cv, r.x + pad, ty, &m, small, col);
        } else if !self.typed.is_empty() && self.typed_at.elapsed() < Duration::from_millis(900) {
            let s = format!("find: {}", self.typed);
            self.text
                .draw(&mut self.cv, r.x + pad, ty, &s, small, p.accent);
        } else if let Some(e) = self.hover.and_then(|h| self.tab().entry(h).cloned()) {
            // The item under the pointer, with its whole name.
            let info = if e.is_dir {
                format!("{} · folder", e.label)
            } else {
                format!(
                    "{} · {} · {}",
                    e.label,
                    dir::human_size(e.size),
                    dir::human_time(e.mtime)
                )
            };
            let info = self.text.ellipsize(&info, small, r.w * 3 / 4);
            self.text
                .draw(&mut self.cv, r.x + pad, ty, &info, small, p.foreground);
        } else {
            self.text
                .draw(&mut self.cv, r.x + pad, ty, &left, small, p.muted);
        }
        let right = "Ctrl+1-4 views · F1 keys";
        let rw = self.text.width(right, small);
        self.text.draw(
            &mut self.cv,
            r.x + r.w - rw - pad,
            ty,
            right,
            small,
            blend(p.border, p.muted, 0.5),
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
        self.submit_as(job, Origin::New);
    }

    fn submit_as(&mut self, job: crate::ops::Job, origin: Origin) {
        if self.running.is_none() {
            self.job_started = Some(Instant::now());
        }
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if self.running.is_none() {
            self.running = Some(Running {
                verb: job.verb(),
                done: 0,
                total: 0,
                current: String::new(),
            });
        }
        self.pending += 1;
        self.jobs.push_back((origin, cancel.clone()));
        let _ = self.worker.tx.send((job, cancel));
        self.dirty = true;
    }

    fn worker_messages(&mut self) {
        while let Ok(d) = self.thumbs.rx.try_recv() {
            if let Some(g) = self.gallery.as_mut() {
                if g.strip.contains_key(&d.path) {
                    let b = (64.0 * self.m.s) as i32;
                    let img = d
                        .image
                        .as_ref()
                        .map(|i| std::rc::Rc::new(crate::icons::scale_to_fit(i, b * 3 / 2, b)));
                    g.strip.insert(d.path.clone(), img);
                    self.dirty = true;
                }
            }
            let b = self.picture_size().max(16);
            let fitted = d
                .image
                .map(|img| std::rc::Rc::new(crate::icons::scale_to_fit(&img, b, b)));
            self.thumb_img.insert(d.path, fitted);
            self.dirty = true;
        }
        while let Ok((path, st)) = self.nc_rx.try_recv() {
            self.nc_status.insert(path, st);
            self.dirty = true;
        }
        while let Ok((gen, found, done)) = self.search_rx.try_recv() {
            if gen != self.search_gen.load(std::sync::atomic::Ordering::Relaxed) {
                continue;
            }
            let hidden = self.hidden;
            let t = self.tab_mut();
            for e in found {
                if hidden || !e.hidden() {
                    t.shown.push(t.all.len());
                    t.selected.push(false);
                }
                t.all.push(e);
            }
            if done {
                self.searching = false;
            }
            self.dirty = true;
        }
        while let Ok((path, img)) = self.gallery_rx.try_recv() {
            if let Some(g) = self.gallery.as_mut() {
                g.images.insert(path, img.map(std::rc::Rc::new));
                self.dirty = true;
            }
        }
        while let Ok(pv) = self.preview_rx.try_recv() {
            if self.preview.as_ref().is_some_and(|cur| cur.path == pv.path) {
                self.preview = Some(pv);
                self.dirty = true;
            }
        }
        while let Ok(m) = self.rx.try_recv() {
            match m {
                crate::ops::Msg::Ask {
                    name,
                    folder,
                    existing,
                    incoming,
                } => {
                    self.ask = Some(Ask {
                        name,
                        folder,
                        existing,
                        incoming,
                        all: false,
                    });
                }
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
                    let origin = self.jobs.pop_front().map_or(Origin::New, |j| j.0);
                    self.ask = None;
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
                        self.remember_undo(u, origin);
                    }
                    self.select_after = created;
                    if let Some(dir) = open {
                        self.refresh_places();
                        self.go(dir);
                    } else if summary == "Safe to remove" {
                        self.refresh_places();
                    }
                    // A long job ends with the NIRUX sound, unless Do not disturb is on.
                    if self.pending == 0
                        && self
                            .job_started
                            .take()
                            .is_some_and(|t| t.elapsed() > Duration::from_secs(5))
                    {
                        play_sound(if errors.is_empty() {
                            "timer-done"
                        } else {
                            "dictation-error"
                        });
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
                self.clip_text = None;
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
                self.clip_text = None;
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
            Act::Rename => self.start_rename(),
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
            Act::NewFolder | Act::NewFile | Act::NewFrom(_) if self.in_trash() => {
                self.flash_error("New items cannot be made in the trash".into());
            }
            Act::NewFolder => {
                let p = crate::ops::free_name(&here, Path::new("New folder"));
                match std::fs::create_dir(&p) {
                    Ok(()) => self.created_new(p),
                    Err(e) => self.flash_error(format!("Could not create the folder: {e}")),
                }
            }
            Act::NewFile => match crate::templates::create_empty(&here) {
                Ok(p) => self.created_new(p),
                Err(e) => self.flash_error(crate::templates::error(&e)),
            },
            Act::NewFrom(k) => {
                let Some(t) = self.menu_templates.get(k as usize).cloned() else {
                    return;
                };
                match crate::templates::create_from(&t, &here) {
                    Ok(p) => self.created_new(p),
                    Err(e) => self.flash_error(crate::templates::error(&e)),
                }
            }
            Act::Terminal => open_terminal(&here),
            Act::Bookmark => match crate::remote::add_bookmark(&here) {
                Ok(true) => {
                    self.flash(format!("Bookmarked {}", places::pretty(&here)));
                    self.refresh_places();
                }
                Ok(false) => self.flash("Already bookmarked".into()),
                Err(e) => self.flash_error(format!("Could not bookmark: {e}")),
            },
            Act::PlaceTab => {
                if let Some(p) = self
                    .menu_place
                    .take()
                    .and_then(|i| self.places.get(i))
                    .map(|p| p.path.clone())
                {
                    if !p.as_os_str().is_empty() {
                        self.new_tab(p);
                    }
                }
            }
            Act::BookmarkUp | Act::BookmarkDown => {
                if let Some(i) = self.menu_place.take() {
                    let rows: Vec<usize> = (0..self.places.len())
                        .filter(|&k| self.places[k].bookmark)
                        .collect();
                    if let Some(k) = rows.iter().position(|&r| r == i) {
                        let other = if a == Act::BookmarkUp {
                            k.checked_sub(1)
                        } else {
                            (k + 1 < rows.len()).then_some(k + 1)
                        };
                        if let Some(o) = other {
                            self.move_bookmark_row(i, rows[o]);
                        }
                    }
                }
            }
            Act::RenameBookmark => {
                if let Some(i) = self.menu_place.take() {
                    if let Some(pl) = self.places.get(i) {
                        // All selected, so typing replaces the label.
                        let ed = crate::edit::LineEdit::selecting(&pl.label, 0, pl.label.len());
                        self.place_rename = Some((i, ed));
                    }
                }
            }
            Act::BookmarkSelected => {
                let dirs: Vec<PathBuf> = self
                    .selection_paths()
                    .into_iter()
                    .filter(|p| p.is_dir())
                    .collect();
                let mut added = 0;
                for d in &dirs {
                    if let Ok(true) = crate::remote::add_bookmark(d) {
                        added += 1;
                    }
                }
                self.flash(match added {
                    0 => "Already bookmarked".into(),
                    1 => "Bookmarked".into(),
                    n => format!("{n} bookmarks added"),
                });
                self.refresh_places();
            }
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
            Act::Props => self.open_props(),
            Act::Wallpaper => {
                if let Some(p) = sel.first() {
                    spawn(
                        "niructl",
                        &[
                            std::ffi::OsStr::new("wallpaper"),
                            std::ffi::OsStr::new("set"),
                            p.as_os_str(),
                        ],
                        None,
                    );
                    self.flash("Set as wallpaper".into());
                }
            }
            Act::SendTo(k) => {
                if let Some(host) = self.menu_hosts.get(k as usize).cloned() {
                    for p in sel.iter().filter(|p| p.is_file()) {
                        spawn(
                            "nirushare",
                            &[
                                std::ffi::OsStr::new("send"),
                                std::ffi::OsStr::new("--to"),
                                std::ffi::OsStr::new(&host),
                                std::ffi::OsStr::new("--file"),
                                p.as_os_str(),
                            ],
                            None,
                        );
                    }
                    self.flash(format!("Sending to {host}"));
                }
            }
            Act::OpenWith(k) => {
                if let Some(app) = self.menu_apps.get(k as usize).cloned() {
                    crate::apps::launch(&app, &sel, &here);
                    self.flash(format!("Opening with {}", app.name));
                }
            }
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
        if target == self.x.atoms.utf8 {
            // Text a text field asked for (Ctrl+V while renaming).
            if property != x11rb::NONE {
                if let Some(data) = self.x.take_selection() {
                    self.insert_in_field(&String::from_utf8_lossy(&data));
                }
            }
            return;
        }
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

    /// A new folder or file: an undo step, then straight into renaming it
    /// with the name before the extension selected.
    fn created_new(&mut self, p: PathBuf) {
        self.undo
            .push(crate::ops::Undo::TrashCreated(vec![p.clone()]));
        let a = self.active;
        self.load_tab(a, false);
        self.select_after = vec![p];
        self.apply_select_after();
        self.start_rename();
    }

    /// An undo step from a finished job.
    fn remember_undo(&mut self, u: crate::ops::Undo, origin: Origin) {
        match origin {
            Origin::New => {
                self.undo.push(u);
                if self.undo.len() > 50 {
                    self.undo.remove(0);
                }
            }
            // Undoing returns how to do it again: kept for redo later.
            Origin::Undo => {}
        }
    }

    /// Ctrl+Z: the last operation taken back on the worker, which also
    /// says how to do it again.
    fn undo_last(&mut self) {
        let Some(undo) = self.undo.pop() else {
            self.flash("Nothing to undo".into());
            return;
        };
        self.submit_as(crate::ops::Job::Revert { undo }, Origin::Undo);
    }

    /// Open a place of the side panel: go there, mount it or connect.
    fn open_place(&mut self, i: usize) {
        if let Some(pl) = self.places.get(i).cloned() {
            if pl.icon == "document-open-recent" {
                self.show_recent();
            } else if pl.connect {
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

    /// Position of a side panel row among the bookmarks (the file order).
    fn bookmark_pos(&self, i: usize) -> Option<usize> {
        self.places.get(i).filter(|p| p.bookmark)?;
        Some(self.places[..i].iter().filter(|p| p.bookmark).count())
    }

    fn move_bookmark_row(&mut self, from: usize, to: usize) {
        if let (Some(a), Some(b)) = (self.bookmark_pos(from), self.bookmark_pos(to)) {
            match crate::remote::move_bookmark(a, b) {
                Ok(()) => self.refresh_places(),
                Err(e) => self.flash_error(format!("Could not move the bookmark: {e}")),
            }
        }
    }

    fn place_motion(&mut self, x: i32, y: i32) {
        let Some((from, py)) = self.place_press else {
            return;
        };
        if self.place_drag_to.is_none() && (y - py).abs() <= (6.0 * self.m.s) as i32 {
            return;
        }
        let to = match self.hit(x.min(self.m.side_w - 1), y) {
            Some(Hit::Place(j) | Hit::Eject(j))
                if self.places.get(j).is_some_and(|p| p.bookmark) =>
            {
                j
            }
            _ => self.place_drag_to.unwrap_or(from),
        };
        if self.place_drag_to != Some(to) {
            self.place_drag_to = Some(to);
            self.dirty = true;
        }
    }

    /// The end of a press on a bookmark: a move when it was dragged,
    /// otherwise a click.
    fn place_release(&mut self) {
        let Some((from, _)) = self.place_press.take() else {
            return;
        };
        match self.place_drag_to.take() {
            Some(to) if to != from => self.move_bookmark_row(from, to),
            Some(_) => {}
            None => self.open_place(from),
        }
        self.dirty = true;
    }

    fn place_rename_key(&mut self, sym: u32, ctrl: bool, shift: bool) {
        let ch = keysym_char(sym);
        let Some((_, ed)) = self.place_rename.as_mut() else {
            return;
        };
        match ed.key(sym, ctrl, shift, ch) {
            EditKey::Commit => self.commit_place_rename(),
            EditKey::Cancel => self.place_rename = None,
            EditKey::Copy => {
                let s = ed.selected().to_string();
                self.copy_text(s);
            }
            EditKey::Cut => {
                if let Some(s) = ed.cut() {
                    self.copy_text(s);
                }
            }
            EditKey::Paste => self.paste_text(),
            _ => {}
        }
        self.dirty = true;
    }

    fn commit_place_rename(&mut self) {
        let Some((i, ed)) = self.place_rename.take() else {
            return;
        };
        let name = ed.text;
        self.dirty = true;
        let Some(pl) = self.places.get(i).cloned() else {
            return;
        };
        if name.trim() == pl.label {
            return;
        }
        let uri = pl
            .uri
            .clone()
            .unwrap_or_else(|| crate::x::file_uri(&pl.path));
        match crate::remote::rename_bookmark(&uri, &name) {
            Ok(()) => self.refresh_places(),
            Err(e) => self.flash_error(format!("Could not rename the bookmark: {e}")),
        }
    }

    /// F2: the name under the cursor in place, several names in a dialog.
    fn start_rename(&mut self) {
        if self.in_trash() {
            // The trash finds an item's way back by its name.
            self.flash_error("Restore it first to rename it".into());
            return;
        }
        let sel = self.selection_paths();
        if sel.len() > 1 {
            return self.start_batch(sel);
        }
        let t = self.tab();
        let Some(e) = t.entry(t.cursor).cloned() else {
            return;
        };
        // The name without its extension is selected, so typing replaces
        // just that, as in other file managers.
        let end = crate::edit::stem_end(&e.name, e.is_dir);
        self.reveal();
        self.rename = Some(Rename {
            path: e.path,
            ed: crate::edit::LineEdit::selecting(&e.name, 0, end),
        });
        self.dirty = true;
    }

    /// The row of the item being renamed, if it is still in the list.
    fn rename_row(&self) -> Option<usize> {
        let r = self.rename.as_ref()?;
        let t = self.tab();
        t.shown.iter().position(|k| t.all[*k].path == r.path)
    }

    fn rename_key(&mut self, sym: u32, ctrl: bool, shift: bool) {
        let ch = keysym_char(sym);
        let Some(rn) = self.rename.as_mut() else {
            return;
        };
        if ch == Some('/') && !ctrl {
            // Say why nothing appears rather than drop it silently.
            self.flash_error("A name cannot contain \"/\"".into());
            return;
        }
        match rn.ed.key(sym, ctrl, shift, ch) {
            EditKey::Commit => {
                self.commit_rename(true);
            }
            EditKey::Cancel => self.rename = None,
            EditKey::Copy => {
                let s = rn.ed.selected().to_string();
                self.copy_text(s);
            }
            EditKey::Cut => {
                if let Some(s) = rn.ed.cut() {
                    self.copy_text(s);
                }
            }
            EditKey::Paste => self.paste_text(),
            EditKey::Next(back) => self.rename_next(back),
            EditKey::Edited | EditKey::Ignored => {}
        }
        self.dirty = true;
    }

    /// Apply the typed name. With `keep_open` (Enter) a name that cannot be
    /// used leaves the field open to correct it; a click elsewhere closes it
    /// either way. True when the field closed.
    fn commit_rename(&mut self, keep_open: bool) -> bool {
        let Some(rn) = self.rename.as_ref() else {
            return true;
        };
        self.dirty = true;
        let from = rn.path.clone();
        let old = from
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Spaces at the ends are nearly always a slip of the finger.
        let name = rn.ed.text.trim().to_string();
        if rn.ed.text == old || name == old {
            self.rename = None;
            return true;
        }
        let to = from.with_file_name(&name);
        let result = crate::edit::check_name(&name).and_then(|()| {
            crate::ops::rename_noreplace(&from, &to)
                .map_err(|e| crate::ops::rename_error(&e, &old, &name))
        });
        match result {
            Ok(()) => {
                self.rename = None;
                self.renamed(vec![(from, to)]);
                true
            }
            Err(msg) => {
                self.flash_error(msg);
                if !keep_open {
                    self.rename = None;
                }
                !keep_open
            }
        }
    }

    /// Tab in the rename field: apply, then rename the next (Shift: the
    /// previous) item, to go through a folder name by name.
    fn rename_next(&mut self, back: bool) {
        if !self.commit_rename(true) {
            return;
        }
        let t = self.tab();
        let n = t.shown.len();
        if n == 0 {
            return;
        }
        let j = if back {
            (t.cursor + n - 1) % n
        } else {
            (t.cursor + 1) % n
        };
        self.set_cursor(j, false, false);
        self.start_rename();
    }

    /// After items were renamed: an undo step, the list read again and the
    /// items selected under their new names.
    fn renamed(&mut self, pairs: Vec<(PathBuf, PathBuf)>) {
        self.undo.push(crate::ops::Undo::MoveBack(
            pairs.iter().map(|(f, t)| (t.clone(), f.clone())).collect(),
        ));
        let a = self.active;
        if self.tabs[a].search.is_some() {
            // Search results are not read again: rename them in place.
            for (f, t) in &pairs {
                if let Some(e) = self.tabs[a].all.iter_mut().find(|e| &e.path == f) {
                    if let Ok(n) = dir::read_one(t, &self.mime) {
                        *e = n;
                    }
                }
            }
        }
        self.load_tab(a, false);
        self.select_after = pairs.into_iter().map(|(_, t)| t).collect();
        self.apply_select_after();
    }

    /// The text field under the pointer, as drawn in the last frame.
    fn field_at(&self, x: i32, y: i32) -> Option<FieldId> {
        self.fields
            .iter()
            .rev()
            .find(|f| f.1.contains(x, y))
            .map(|f| f.0)
    }

    /// The editor behind a field, if that field is open.
    fn field_edit(&mut self, id: FieldId) -> Option<&mut crate::edit::LineEdit> {
        match id {
            FieldId::Rename => self.rename.as_mut().map(|r| &mut r.ed),
            FieldId::Bookmark => self.place_rename.as_mut().map(|(_, ed)| ed),
            FieldId::Path => self.path_edit.as_mut(),
            FieldId::Search => self.search_edit.as_mut(),
            FieldId::Connect(k) => self.connect.as_mut().map(|f| match k {
                0 => &mut f.uri,
                1 => &mut f.user,
                _ => &mut f.pass,
            }),
            FieldId::Batch(k) => {
                self.batch
                    .as_mut()
                    .map(|b| if k == 0 { &mut b.find } else { &mut b.replace })
            }
        }
    }

    /// The text offset under pointer column `x` in a field. The password
    /// is drawn as dots, so its offset is counted in characters.
    fn field_offset(&mut self, id: FieldId, x: i32, word: bool) -> Option<usize> {
        let origin = self.fields.iter().find(|f| f.0 == id)?.2;
        let font = self.m.font;
        let text = self.field_edit(id)?.text.clone();
        let shown = match id {
            FieldId::Connect(2) => mask(&text),
            _ => text.clone(),
        };
        let at = if word {
            self.text.char_at(&shown, font, x - origin)
        } else {
            self.text.offset_at(&shown, font, x - origin)
        };
        Some(match id {
            FieldId::Connect(2) => unmask(&text, at),
            _ => at,
        })
    }

    /// A press in a text field: one press places the cursor (Shift
    /// extends), two select a word, three the whole text. A field of a
    /// dialog also takes the focus.
    fn field_press(&mut self, id: FieldId, x: i32, shift: bool) {
        // A press in another field ends a rename, as a press elsewhere does.
        if id != FieldId::Rename && self.rename.is_some() {
            self.commit_rename(false);
        }
        if id != FieldId::Bookmark && self.place_rename.is_some() {
            self.commit_place_rename();
        }
        match id {
            FieldId::Connect(k) => {
                if let Some(f) = self.connect.as_mut() {
                    f.field = k;
                }
            }
            FieldId::Batch(k) => {
                if let Some(b) = self.batch.as_mut() {
                    b.field = k;
                }
            }
            _ => {}
        }
        let n = match self.field_clicks {
            Some((t, n, f)) if f == id && t.elapsed() < DOUBLE_CLICK => n % 3 + 1,
            _ => 1,
        };
        self.field_clicks = Some((Instant::now(), n, id));
        let pos = self.field_offset(id, x, n == 2);
        if let (Some(pos), Some(ed)) = (pos, self.field_edit(id)) {
            match n {
                2 => ed.select_word_at(pos),
                3 => ed.select_all(),
                _ => ed.move_to(pos, shift),
            }
        }
        self.field_drag = (n == 1).then_some(id);
        self.dirty = true;
    }

    /// The button held after a press in a field: select to the pointer.
    fn field_drag_to(&mut self, x: i32) {
        let Some(id) = self.field_drag else {
            return;
        };
        let pos = self.field_offset(id, x, false);
        if let (Some(pos), Some(ed)) = (pos, self.field_edit(id)) {
            if pos != ed.cursor {
                ed.move_to(pos, true);
                self.dirty = true;
            }
        }
    }

    /// Text from a field onto the clipboard, in place of any files.
    fn copy_text(&mut self, s: String) {
        if s.is_empty() {
            return;
        }
        self.clip = None;
        self.clip_text = Some(s);
        let _ = self.x.own_clipboard();
    }

    /// Ctrl+V in a text field: our own clipboard at once (files give their
    /// names), another program's when its answer comes (see `pasted`).
    fn paste_text(&mut self) {
        if !self.x.owns_clipboard() {
            let _ = self.x.request_clipboard(self.x.atoms.utf8);
            return;
        }
        let s = match (&self.clip, &self.clip_text) {
            (Some((files, _)), _) => files
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" "),
            (None, Some(t)) => t.clone(),
            (None, None) => return,
        };
        self.insert_in_field(&s);
    }

    /// Pasted text into whichever text field is open.
    fn insert_in_field(&mut self, s: &str) {
        use crate::edit::one_line;
        if let Some(rn) = self.rename.as_mut() {
            rn.ed.insert(&one_line(s, true));
        } else if let Some(b) = self.batch.as_mut() {
            let ed = if b.field == 0 {
                &mut b.find
            } else {
                &mut b.replace
            };
            ed.insert(&one_line(s, true));
            b.note = None;
        } else if let Some((_, ed)) = self.place_rename.as_mut() {
            ed.insert(&one_line(s, false));
        } else if let Some(f) = self.connect.as_mut() {
            let ed = match f.field {
                0 => &mut f.uri,
                1 => &mut f.user,
                _ => &mut f.pass,
            };
            ed.insert(&one_line(s, false));
        } else if let Some(ed) = self.search_edit.as_mut() {
            ed.insert(&one_line(s, false));
            self.search_due = Some(Instant::now() + Duration::from_millis(180));
        } else if let Some(ed) = self.path_edit.as_mut() {
            ed.insert(&one_line(s, false));
        }
        self.dirty = true;
    }

    fn start_batch(&mut self, paths: Vec<PathBuf>) {
        let items = paths
            .iter()
            .map(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (name, p.is_dir())
            })
            .collect();
        self.batch = Some(BatchRename {
            paths,
            items,
            find: crate::edit::LineEdit::default(),
            replace: crate::edit::LineEdit::default(),
            field: 0,
            note: None,
        });
        self.dirty = true;
    }

    /// Each item's new path and what stops it, if anything.
    fn batch_plan(b: &BatchRename) -> Vec<(PathBuf, PathBuf, Option<String>)> {
        use std::collections::{HashMap, HashSet};
        let names = crate::edit::batch_names(&b.items, &b.find.text, &b.replace.text, 1);
        let targets: Vec<PathBuf> = b
            .paths
            .iter()
            .zip(&names)
            .map(|(p, n)| p.with_file_name(n.trim()))
            .collect();
        let mut count: HashMap<&PathBuf, usize> = HashMap::new();
        for t in &targets {
            *count.entry(t).or_default() += 1;
        }
        let sources: HashSet<&PathBuf> = b.paths.iter().collect();
        b.paths
            .iter()
            .zip(&targets)
            .zip(&names)
            .map(|((from, to), name)| {
                let problem = if count[to] > 1 {
                    Some("two items would get this name".to_string())
                } else if from == to {
                    None
                } else if let Err(e) = crate::edit::check_name(name.trim()) {
                    Some(e)
                } else if !sources.contains(to)
                    && std::fs::symlink_metadata(to).is_ok()
                    && !crate::ops::same_file(from, to)
                {
                    Some("that name exists already".to_string())
                } else {
                    None
                };
                (from.clone(), to.clone(), problem)
            })
            .collect()
    }

    fn batch_key(&mut self, sym: u32, ctrl: bool, shift: bool) {
        let ch = keysym_char(sym);
        let Some(b) = self.batch.as_mut() else {
            return;
        };
        self.dirty = true;
        if ch == Some('/') && !ctrl {
            b.note = Some("A name cannot contain \"/\"".into());
            return;
        }
        let ed = if b.field == 0 {
            &mut b.find
        } else {
            &mut b.replace
        };
        match ed.key(sym, ctrl, shift, ch) {
            EditKey::Commit => self.apply_batch(),
            EditKey::Cancel => self.batch = None,
            EditKey::Next(_) => b.field = 1 - b.field,
            EditKey::Copy => {
                let s = ed.selected().to_string();
                self.copy_text(s);
            }
            EditKey::Cut => {
                if let Some(s) = ed.cut() {
                    self.copy_text(s);
                }
            }
            EditKey::Paste => self.paste_text(),
            EditKey::Edited => b.note = None,
            EditKey::Ignored => {}
        }
    }

    fn apply_batch(&mut self) {
        let Some(b) = self.batch.as_mut() else {
            return;
        };
        let plan = Self::batch_plan(b);
        let bad: Vec<&(PathBuf, PathBuf, Option<String>)> =
            plan.iter().filter(|x| x.2.is_some()).collect();
        if let Some((_, to, Some(why))) = bad.first() {
            let name = to.file_name().unwrap_or_default().to_string_lossy();
            let more = match bad.len() {
                1 => String::new(),
                n => format!(" (and {} more)", n - 1),
            };
            b.note = Some(format!("\"{name}\": {why}{more}"));
            return;
        }
        let moves: Vec<(PathBuf, PathBuf)> = plan
            .into_iter()
            .filter(|(f, t, _)| f != t)
            .map(|(f, t, _)| (f, t))
            .collect();
        if moves.is_empty() {
            b.note = Some("No name changes: type what to find, or what to put instead".into());
            return;
        }
        self.batch = None;
        match crate::ops::rename_all(&moves) {
            Ok(()) => {
                let n = moves.len();
                self.renamed(moves);
                self.error = false;
                self.flash(format!("Renamed {n} item{}", if n == 1 { "" } else { "s" }));
            }
            Err((done, msg)) => {
                if !done.is_empty() {
                    self.renamed(done);
                }
                self.flash_error(msg);
            }
        }
    }

    fn open_menu(&mut self, x: i32, y: i32, on_items: bool) {
        let sel = self.selection_paths();
        let mut items: Vec<(String, Act, &'static str)> = Vec::new();
        let trash = self.in_trash();
        if on_items && !sel.is_empty() {
            items.push(("Open".into(), Act::Open, "Enter"));
            // "Open with" for files of one type: up to six programs.
            let t = self.tab();
            let mimes: Vec<String> = sel
                .iter()
                .filter_map(|p| t.all.iter().find(|e| &e.path == p))
                .filter(|e| !e.is_dir)
                .map(|e| e.mime.clone())
                .collect();
            self.menu_apps.clear();
            if !mimes.is_empty() && mimes.iter().all(|m| m == &mimes[0]) {
                let apps = self.apps.get_or_insert_with(crate::apps::Apps::load);
                self.menu_apps = apps.for_mime(&mimes[0]).into_iter().take(6).collect();
                for (k, a) in self.menu_apps.iter().enumerate() {
                    items.push((format!("Open with {}", a.name), Act::OpenWith(k as u8), ""));
                }
            }
            if sel.iter().any(|p| p.is_dir()) {
                items.push(("Open in a new tab".into(), Act::OpenTab, ""));
                items.push(("Add to bookmarks".into(), Act::BookmarkSelected, ""));
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
                } else {
                    items.push(("Rename…".into(), Act::Rename, "F2"));
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
                if sel.len() == 1 && mimes_image(&self.mime, &sel[0]) {
                    items.push(("Set as wallpaper".into(), Act::Wallpaper, "W in gallery"));
                }
                self.menu_hosts = self
                    .share_hosts
                    .lock()
                    .map(|h| h.clone())
                    .unwrap_or_default();
                self.menu_hosts.truncate(4);
                if sel.iter().all(|p| p.is_file()) {
                    for (k, h) in self.menu_hosts.iter().enumerate() {
                        items.push((format!("Send to {h}"), Act::SendTo(k as u8), "NIRUSHARE"));
                    }
                }
                self.refresh_share_hosts();
                items.push(("Properties".into(), Act::Props, "Alt+Enter"));
            }
        } else {
            if !trash {
                items.push(("Paste".into(), Act::Paste, "Ctrl+V"));
                items.push(("New folder".into(), Act::NewFolder, "Ctrl+Shift+N"));
                items.push(("New empty file".into(), Act::NewFile, "Ctrl+Alt+N"));
                self.menu_templates = crate::templates::dir()
                    .map(|d| crate::templates::list(&d))
                    .unwrap_or_default();
                self.menu_templates.truncate(crate::templates::MAX);
                for (k, t) in self.menu_templates.iter().enumerate() {
                    let name = t.file_name().unwrap_or_default().to_string_lossy();
                    items.push((format!("New {name}"), Act::NewFrom(k as u8), "template"));
                }
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

    /// Where the rename field of row `i` goes: over the name. In the icon
    /// views it widens for a long name, up to three cells.
    fn rename_field(&mut self, i: usize) -> Rect {
        let (font, pad) = (self.m.font, self.m.pad);
        let r = self.item_rect(i);
        let (_, _, pic) = self.grid();
        let fr = match self.view {
            View::List => {
                let x = r.x + 2 * pad + self.m.small_icon - pad / 2;
                let w = (self.list_name_w() - self.m.small_icon - pad).max(r.w / 3);
                Rect::new(x, r.y + 1, w, r.h - 2)
            }
            View::Compact => {
                Rect::new(r.x + pad + pic - pad / 2, r.y + 1, r.w - pic - pad, r.h - 2)
            }
            _ => {
                let tw = match self.rename.as_ref() {
                    Some(rn) => self.text.width(&rn.ed.text, font),
                    None => 0,
                };
                let w = (tw + 2 * pad).clamp(r.w + 2 * pad, 3 * r.w);
                Rect::new(
                    r.x + r.w / 2 - w / 2,
                    r.y + pad + pic + pad / 4,
                    w,
                    font.height + pad / 2,
                )
            }
        };
        // Inside the folder area: moved in from its right edge, then cut.
        let c = self.content;
        let x = fr.x.min(c.x + c.w - fr.w).max(c.x);
        Rect::new(x, fr.y, fr.w.min(c.x + c.w - x), fr.h)
    }

    /// The rename field over an item's label.
    fn draw_rename(&mut self) {
        if self.rename.is_none() {
            return;
        }
        let Some(i) = self.rename_row() else {
            // Moved or removed by another program while typing.
            self.rename = None;
            self.flash_error("The item was moved or removed meanwhile".into());
            return;
        };
        let p = self.p;
        let fr = self.rename_field(i);
        self.cv.set_clip(self.content);
        self.cv.fill(fr, p.background);
        self.cv.frame(fr, p.accent);
        self.cv.reset_clip();
        if let Some(rn) = self.rename.take() {
            let clip = fr.inset(1).intersect(&self.content);
            self.draw_edit(FieldId::Rename, (fr, clip), &rn.ed, true);
            self.rename = Some(rn);
        }
    }

    /// A text field's text, selection and cursor in `fr`, drawn inside
    /// `clip` and scrolled sideways so the cursor shows. The field is
    /// remembered as `id`, so a press in it can place the cursor and the
    /// next frame keeps its scroll.
    fn draw_edit(
        &mut self,
        id: FieldId,
        (fr, clip): (Rect, Rect),
        ed: &crate::edit::LineEdit,
        focus: bool,
    ) {
        let scroll = self
            .fields_prev
            .iter()
            .find(|f| f.0 == id)
            .map_or(0, |f| f.3);
        let p = self.p;
        let (font, pad) = (self.m.font, self.m.pad);
        let inner = (fr.w - pad).max(1);
        let total = self.text.width(&ed.text, font);
        let at = |t: &mut Text, i: usize| t.width(&ed.text[..i], font);
        let cx = at(&mut self.text, ed.cursor);
        // No empty room after the end of the text while it is scrolled.
        let mut scroll = scroll.min((total + 2 - inner).max(0));
        if cx - scroll > inner - 2 {
            scroll = cx - inner + 2;
        }
        scroll = scroll.min(cx).max(0);
        let (tx, ty) = (fr.x + pad / 2 - scroll, fr.y + (fr.h - font.height) / 2);
        self.fields.push((id, clip, tx, scroll));
        self.cv.set_clip(clip);
        if ed.has_selection() {
            // Tinted with the accent: the item's own selection colour is
            // too close to the field's background in dark themes.
            let mark = blend(p.selected, p.accent, 0.35);
            let (a, b) = ed.selection();
            let (xa, xb) = (at(&mut self.text, a), at(&mut self.text, b));
            self.cv
                .fill(Rect::new(tx + xa, ty, xb - xa, font.height), mark);
        }
        self.text
            .draw(&mut self.cv, tx, ty, &ed.text, font, p.foreground);
        if focus {
            let w = (1.5 * self.m.s).round().max(1.0) as i32;
            self.cv
                .fill(Rect::new(tx + cx, ty, w, font.height), p.accent);
        }
        self.cv.reset_clip();
    }

    /// The dialog for renaming several items, with the new names ahead.
    fn draw_batch(&mut self) {
        let Some(b) = self.batch.take() else {
            return;
        };
        let p = self.p;
        let (font, small, pad) = (self.m.font, self.m.small, self.m.pad);
        let plan = Self::batch_plan(&b);
        let rows = plan.len().min(8) as i32;
        let w = (self.cv.w * 2 / 3)
            .clamp((420.0 * self.m.s) as i32, (720.0 * self.m.s) as i32)
            .min(self.cv.w - 2 * pad);
        let field_h = font.height + pad;
        let h = 4 * pad
            + font.height
            + 2 * (small.height + pad / 3 + field_h + pad)
            + (rows + 1) * (small.height + pad / 3)
            + 2 * small.height
            + 3 * pad;
        let r = Rect::new((self.cv.w - w) / 2, ((self.cv.h - h) / 3).max(0), w, h);
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
            "RENAME",
            (2.0 * self.m.s).round().max(2.0) as i32,
            p.accent,
            None,
        );
        y += font.height + pad;
        let labels = [
            "Find (empty: the whole name before the extension)",
            "Replace with ({n} a number, {name} the old name)",
        ];
        for (k, label) in labels.iter().enumerate() {
            let label = self.text.ellipsize(label, small, r.w - 4 * pad);
            self.text
                .draw(&mut self.cv, r.x + 2 * pad, y, &label, small, p.muted);
            y += small.height + pad / 3;
            let fr = Rect::new(r.x + 2 * pad, y, r.w - 4 * pad, field_h);
            self.cv.fill(fr, p.background);
            self.cv
                .frame(fr, if k == b.field { p.accent } else { p.border });
            let ed = if k == 0 { &b.find } else { &b.replace };
            self.draw_edit(FieldId::Batch(k), (fr, fr.inset(1)), ed, k == b.field);
            y += field_h + pad;
        }
        // The first new names, as they will be.
        let col = (r.w - 4 * pad) / 2;
        for (from, to, problem) in plan.iter().take(rows as usize) {
            let old = from.file_name().unwrap_or_default().to_string_lossy();
            let new = to.file_name().unwrap_or_default().to_string_lossy();
            let old = self.text.ellipsize(&old, small, col - pad);
            let new = self.text.ellipsize(&format!("→ {new}"), small, col);
            let c = if problem.is_some() {
                p.error
            } else if from == to {
                p.muted
            } else {
                p.foreground
            };
            self.text
                .draw(&mut self.cv, r.x + 2 * pad, y, &old, small, p.muted);
            self.text
                .draw(&mut self.cv, r.x + 2 * pad + col, y, &new, small, c);
            y += small.height + pad / 3;
        }
        let bad = plan.iter().filter(|x| x.2.is_some()).count();
        let mut more = match plan.len() as i32 - rows {
            0 => String::new(),
            n => format!("and {n} more"),
        };
        if bad > 0 {
            more = format!(
                "{more}{}{bad} with a problem",
                if more.is_empty() { "" } else { " · " }
            );
        }
        self.text
            .draw(&mut self.cv, r.x + 2 * pad, y, &more, small, p.muted);
        y += small.height + pad;
        let (note, warn) = match &b.note {
            Some(n) => (n.clone(), true),
            None => (
                "Folders keep the whole name; extensions stay as they are".into(),
                false,
            ),
        };
        let note = self.text.ellipsize(&note, small, r.w - 4 * pad);
        self.text.draw(
            &mut self.cv,
            r.x + 2 * pad,
            y,
            &note,
            small,
            if warn { p.warning } else { p.muted },
        );
        y += small.height + pad / 2;
        self.text.draw(
            &mut self.cv,
            r.x + 2 * pad,
            y,
            &format!(
                "Enter rename {} items · Tab other field · Esc cancel",
                b.paths.len()
            ),
            small,
            p.muted,
        );
        self.batch = Some(b);
    }
}

impl App {
    /// A thumbnail when there is one; asks for it the first time.
    fn thumb_for(&mut self, e: &Entry) -> Option<std::rc::Rc<crate::canvas::Image>> {
        if e.is_dir
            || !crate::thumbs::wanted(&e.mime, &self.thumb_types)
            || self.picture_size() < 40
        {
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
            size: self.picture_size().max(1) as u32,
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
        self.remember_folder();
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
        let cols = if self.view != View::List {
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
            uri: crate::edit::LineEdit::new(&uri),
            user: crate::edit::LineEdit::new(&user),
            pass: crate::edit::LineEdit::default(),
            field: if login { 2 } else { 0 },
            login,
            note,
        });
        self.dirty = true;
    }

    fn connect_key(&mut self, sym: u32, ctrl: bool, shift: bool) {
        let ch = keysym_char(sym);
        let Some(f) = self.connect.as_mut() else {
            return;
        };
        self.dirty = true;
        let fields = if f.login { 3 } else { 1 };
        let ed = match f.field {
            0 => &mut f.uri,
            1 => &mut f.user,
            _ => &mut f.pass,
        };
        match ed.key(sym, ctrl, shift, ch) {
            EditKey::Cancel => self.connect = None,
            EditKey::Next(back) => {
                // Tab: complete a host from ~/.ssh/config, or move field.
                let typed = f.uri.text.strip_prefix("sftp://").map(str::to_string);
                if let (0, Some(typed), false) = (f.field, typed, back) {
                    let hosts = crate::remote::ssh_hosts();
                    let mut hits: Vec<&String> =
                        hosts.iter().filter(|h| h.starts_with(&typed)).collect();
                    hits.sort();
                    if let Some(h) = hits.first() {
                        f.uri.select_all();
                        f.uri.insert(&format!("sftp://{h}"));
                    }
                } else if fields > 1 {
                    f.field = if back {
                        (f.field + fields - 1) % fields
                    } else {
                        (f.field + 1) % fields
                    };
                }
            }
            EditKey::Commit => {
                let uri = f.uri.text.trim().to_string();
                if !uri.contains("://") || uri.ends_with("://") {
                    f.note = Some(
                        "Type an address like sftp://host, ftp://host or smb://host/share".into(),
                    );
                } else {
                    let login = f.login.then(|| (f.user.text.clone(), f.pass.text.clone()));
                    self.connect = None;
                    self.submit(crate::ops::Job::Connect { uri, login });
                }
            }
            // A password is never copied out of its field.
            EditKey::Copy | EditKey::Cut if f.field == 2 => {}
            EditKey::Copy => {
                let s = ed.selected().to_string();
                self.copy_text(s);
            }
            EditKey::Cut => {
                if let Some(s) = ed.cut() {
                    self.copy_text(s);
                }
            }
            EditKey::Paste => self.paste_text(),
            EditKey::Edited | EditKey::Ignored => {}
        }
    }

    fn draw_connect(&mut self) {
        let Some(f) = self.connect.as_ref() else {
            return;
        };
        let p = self.p;
        let (font, small, pad) = (self.m.font, self.m.small, self.m.pad);
        let (eds, field, login, note) = (
            [f.uri.clone(), f.user.clone(), masked(&f.pass)],
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
            self.draw_edit(FieldId::Connect(k), (fr, fr.inset(1)), &eds[k], k == field);
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

impl App {
    fn is_xdnd(&self, t: u32) -> bool {
        let a = &self.x.atoms;
        [
            a.xdnd_enter,
            a.xdnd_position,
            a.xdnd_status,
            a.xdnd_leave,
            a.xdnd_drop,
            a.xdnd_finished,
        ]
        .contains(&t)
    }

    /// Where a drop at window position (x, y) lands, if anywhere.
    fn drop_at(&self, x: i32, y: i32, dragged: &[PathBuf]) -> Option<DropAt> {
        let ok = |p: &Path| !dragged.iter().any(|d| p.starts_with(d));
        match self.hit(x, y)? {
            Hit::Item(i) => {
                let e = self.tab().entry(i)?;
                if e.is_dir {
                    ok(&e.path).then_some(DropAt::Item(i))
                } else {
                    Some(DropAt::Here)
                }
            }
            Hit::Place(i) => {
                let pl = self.places.get(i)?;
                (pl.mounted && !pl.connect && ok(&pl.path)).then_some(DropAt::Place(i))
            }
            Hit::Tab(i) | Hit::TabClose(i) | Hit::Pane(i) => {
                ok(&self.tabs.get(i)?.path).then_some(DropAt::Tab(i))
            }
            Hit::Content | Hit::Header(_) => Some(DropAt::Here),
            _ => None,
        }
    }

    fn drop_dir(&self, at: &DropAt) -> Option<PathBuf> {
        match at {
            DropAt::Item(i) => self.tab().entry(*i).map(|e| e.path.clone()),
            DropAt::Place(i) => self.places.get(*i).map(|p| p.path.clone()),
            DropAt::Tab(i) => self.tabs.get(*i).map(|t| t.path.clone()),
            DropAt::Here => Some(self.tab().path.clone()),
        }
    }

    fn drag_motion(&mut self, x: i32, y: i32, rx: i16, ry: i16, state: KeyButMask) {
        if self.drag.is_none() {
            let Some((px, py)) = self.press else { return };
            let far = (x - px).abs() > (6.0 * self.m.s) as i32
                || (y - py).abs() > (6.0 * self.m.s) as i32;
            if !far {
                return;
            }
            // Start dragging the selection.
            let files = self.selection_paths();
            if files.is_empty() {
                self.press = None;
                return;
            }
            let a = &self.x.atoms;
            let _ = self
                .x
                .c
                .set_selection_owner(self.x.win, a.xdnd_selection, x11rb::CURRENT_TIME);
            let _ = self.x.c.change_property32(
                PropMode::REPLACE,
                self.x.win,
                a.xdnd_type_list,
                AtomEnum::ATOM,
                &[a.uri_list, a.utf8],
            );
            self.x.grab();
            self.single_on_release = None;
            self.drag = Some(DragOut {
                files,
                target: None,
                accepted: false,
                finished_wait: None,
            });
        }
        let target = self.x.xdnd_target(rx, ry);
        let own = target.is_some_and(|(w, _)| w == self.x.win);
        let files = self
            .drag
            .as_ref()
            .map(|d| d.files.clone())
            .unwrap_or_default();
        let prev = self.drag.as_ref().and_then(|d| d.target);
        let a = (
            self.x.atoms.xdnd_enter,
            self.x.atoms.xdnd_leave,
            self.x.atoms.xdnd_position,
            self.x.atoms.uri_list,
            self.x.atoms.utf8,
        );
        let win = self.x.win;
        if prev.map(|p| p.0) != target.map(|t| t.0) {
            if let Some((w, _)) = prev.filter(|p| p.0 != win) {
                self.x.send_client(w, a.1, [win, 0, 0, 0, 0]);
            }
            if let Some((w, v)) = target.filter(|t| t.0 != win) {
                self.x.send_client(w, a.0, [win, v << 24, a.3, a.4, 0]);
            }
            if let Some(d) = self.drag.as_mut() {
                d.target = target;
                d.accepted = false;
            }
        }
        self.pointer = (x, y);
        if own {
            let at = self.drop_at(x, y, &files);
            self.set_drop_target(at);
            self.edge = if self.content.x <= x && x < self.content.x + self.content.w {
                self.edge_speed_at(y)
            } else {
                0
            };
        } else {
            self.edge = 0;
            self.spring = None;
            if self.drop_target.take().is_some() {
                self.dirty = true;
            }
            if let Some((w, _)) = target {
                let copy = !state.contains(KeyButMask::SHIFT);
                let action = if copy {
                    self.x.atoms.xdnd_copy
                } else {
                    self.x.atoms.xdnd_move
                };
                let pos = ((rx as u16 as u32) << 16) | ry as u16 as u32;
                self.x.send_client(w, a.2, [win, 0, pos, 0, action]);
            }
        }
        self.drag_label(rx, ry, state);
    }

    fn release(&mut self, x: i32, y: i32, _rx: i16, _ry: i16, state: KeyButMask) {
        self.press = None;
        let Some(d) = self.drag.take() else {
            if let Some(i) = self.single_on_release.take() {
                self.set_cursor(i, false, false);
            }
            return;
        };
        self.x.ungrab();
        self.hide_drag_label();
        self.spring = None;
        self.edge = 0;
        self.edge_next = None;
        let win = self.x.win;
        match d.target {
            Some((w, _)) if w == win => {
                let at = self.drop_target.take();
                if let Some(dest) = at.and_then(|a| self.drop_dir(&a)) {
                    let ctrl = state.contains(KeyButMask::CONTROL);
                    let shift = state.contains(KeyButMask::SHIFT);
                    self.drop_files(d.files, dest, ctrl, shift);
                }
                let _ = (x, y);
            }
            Some((w, _)) if d.accepted => {
                self.x
                    .send_client(w, self.x.atoms.xdnd_drop, [win, 0, 0, 0, 0]);
                // Keep answering XdndSelection requests for these files.
                self.dropped_files = Some(d.files);
                let _ = d.finished_wait;
            }
            Some((w, _)) => self
                .x
                .send_client(w, self.x.atoms.xdnd_leave, [win, 0, 0, 0, 0]),
            None => {}
        }
        self.dirty = true;
    }

    /// Files dropped on `dest`: moved within one file system, copied across
    /// (Ctrl copies, Shift moves, like other file managers).
    fn drop_files(&mut self, files: Vec<PathBuf>, dest: PathBuf, ctrl: bool, shift: bool) {
        use std::os::unix::fs::MetadataExt;
        let files: Vec<PathBuf> = files
            .into_iter()
            .filter(|f| f.parent() != Some(dest.as_path()))
            .collect();
        if files.is_empty() {
            return;
        }
        let same_fs = std::fs::metadata(&dest).ok().map(|m| m.dev())
            == std::fs::symlink_metadata(&files[0]).ok().map(|m| m.dev());
        let mv = shift || (same_fs && !ctrl);
        if mv {
            self.submit(crate::ops::Job::Move {
                srcs: files,
                dst: dest,
            });
        } else {
            self.submit(crate::ops::Job::Copy {
                srcs: files,
                dst: dest,
            });
        }
    }

    fn xdnd_message(&mut self, t: u32, d: [u32; 5]) {
        let a = (
            self.x.atoms.xdnd_enter,
            self.x.atoms.xdnd_position,
            self.x.atoms.xdnd_status,
            self.x.atoms.xdnd_leave,
            self.x.atoms.xdnd_drop,
            self.x.atoms.xdnd_finished,
        );
        let win = self.x.win;
        if t == a.2 {
            // XdndStatus: the program under the pointer would take the drop.
            if let Some(dr) = self.drag.as_mut() {
                dr.accepted = d[1] & 1 == 1;
            }
        } else if t == a.5 {
            self.dropped_files = None;
        } else if t == a.0 {
            let source = d[0];
            let version = d[1] >> 24;
            let mut types: Vec<u32> = d[2..5].iter().copied().filter(|x| *x != 0).collect();
            if d[1] & 1 == 1 {
                if let Some(v) = self
                    .x
                    .c
                    .get_property(
                        false,
                        source,
                        self.x.atoms.xdnd_type_list,
                        AtomEnum::ATOM,
                        0,
                        64,
                    )
                    .ok()
                    .and_then(|c| c.reply().ok())
                {
                    types = v.value32().map(|i| i.collect()).unwrap_or_default();
                }
            }
            let files_offered = types.contains(&self.x.atoms.uri_list);
            self.drag_in = Some(DragIn {
                source,
                version,
                files_offered,
                move_offered: false,
            });
        } else if t == a.1 {
            let Some(din) = self.drag_in.as_mut() else {
                return;
            };
            din.move_offered = d[4] == self.x.atoms.xdnd_move;
            let (rx, ry) = ((d[2] >> 16) as i16, (d[2] & 0xffff) as i16);
            let (wx, wy) = self.x.to_window(rx, ry);
            let (src, offered) = (din.source, din.files_offered);
            let mv = din.move_offered;
            let at = if offered {
                self.drop_at(wx, wy, &[])
            } else {
                None
            };
            self.pointer = (wx, wy);
            self.edge = if at.is_some() {
                self.edge_speed_at(wy)
            } else {
                0
            };
            let accept = u32::from(at.is_some());
            self.set_drop_target(at);
            let action = if mv {
                self.x.atoms.xdnd_move
            } else {
                self.x.atoms.xdnd_copy
            };
            // Bit 1: keep sending positions, the target changes per item.
            self.x
                .send_client(src, a.2, [win, accept | 2, 0, 0, action]);
        } else if t == a.3 {
            self.drag_in = None;
            self.spring = None;
            self.edge = 0;
            if self.drop_target.take().is_some() {
                self.dirty = true;
            }
        } else if t == a.4 {
            let time = d[2];
            if self.drag_in.as_ref().is_some_and(|x| x.files_offered) && self.drop_target.is_some()
            {
                let _ = self.x.c.convert_selection(
                    win,
                    self.x.atoms.xdnd_selection,
                    self.x.atoms.uri_list,
                    self.x.atoms.dnd_prop,
                    time,
                );
                let _ = self.x.c.flush();
            } else if let Some(din) = self.drag_in.take() {
                self.x.send_client(din.source, a.5, [win, 0, 0, 0, 0]);
                self.drop_target = None;
                self.dirty = true;
            }
        }
    }

    /// The dropped data arrived: copy the files where they were dropped.
    fn drop_data(&mut self, property: u32) {
        let din = self.drag_in.take();
        let at = self.drop_target.take();
        self.spring = None;
        self.edge = 0;
        self.dirty = true;
        // The other program asked for a move (usually Shift held there).
        let mv = din.as_ref().is_some_and(|d| d.move_offered);
        let win = self.x.win;
        let mut ok = false;
        if property != x11rb::NONE {
            let data = self
                .x
                .c
                .get_property(true, win, self.x.atoms.dnd_prop, AtomEnum::ANY, 0, 1 << 24)
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.value);
            if let (Some(data), Some(dest)) = (data, at.and_then(|a| self.drop_dir(&a))) {
                let (files, _) = crate::x::parse_uris(&data);
                if !files.is_empty() {
                    ok = true;
                    self.drop_files(files, dest, !mv, mv);
                }
            }
        }
        if let Some(d) = din {
            let _ = d.version;
            let action = match (ok, mv) {
                (false, _) => 0,
                (true, true) => self.x.atoms.xdnd_move,
                (true, false) => self.x.atoms.xdnd_copy,
            };
            self.x.send_client(
                d.source,
                self.x.atoms.xdnd_finished,
                [win, u32::from(ok), action, 0, 0],
            );
        }
    }

    /// A strong accent frame on the folder a drop would land in.
    fn draw_drop_target(&mut self) {
        let Some(at) = self.drop_target.clone() else {
            return;
        };
        let p = self.p;
        let w = (2.0 * self.m.s).max(2.0) as i32;
        let r = match at {
            DropAt::Item(i) => self.item_rect(i).intersect(&self.content),
            DropAt::Here => self.content,
            DropAt::Place(i) => match self
                .hits
                .iter()
                .find(|(_, h)| matches!(h, Hit::Place(k) if *k == i))
            {
                Some((r, _)) => *r,
                None => return,
            },
            DropAt::Tab(i) => match self
                .hits
                .iter()
                .find(|(_, h)| matches!(h, Hit::Tab(k) if *k == i))
            {
                Some((r, _)) => *r,
                None => return,
            },
        };
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        self.cv.tint(r, p.accent, 40);
        for k in 0..w {
            self.cv.frame(r.inset(k), p.accent);
        }
    }
}

impl App {
    /// The view this folder had last time, or a picture view for a folder of
    /// photos and films.
    fn apply_memory(&mut self, path: &Path) {
        let (view, zoom, sort, reverse) = match self.folders.get(path) {
            Some(v) => *v,
            None => {
                let mut media = 0usize;
                let mut total = 0usize;
                if let Ok(rd) = std::fs::read_dir(path) {
                    for e in rd.flatten().take(400) {
                        let n = e.file_name().to_string_lossy().into_owned();
                        if n.starts_with('.') {
                            continue;
                        }
                        total += 1;
                        let m = self.mime.for_name(&n);
                        if m.starts_with("image/") || m.starts_with("video/") {
                            media += 1;
                        }
                    }
                }
                let photos = total >= 4 && media * 2 >= total;
                (
                    if photos { View::Thumbs } else { View::Icons },
                    ZOOM_DEFAULT,
                    SortKey::Name,
                    false,
                )
            }
        };
        if view != self.view {
            self.view = view;
        }
        if zoom != self.zoom {
            self.zoom = zoom.min(ZOOM.len() - 1);
            self.m = metrics(&self.text, self.font_px, self.m.s, self.zoom);
        }
        self.sort = sort;
        self.reverse = reverse;
    }

    fn remember_folder(&mut self) {
        let path = self.tab().path.clone();
        self.folders
            .insert(path, (self.view, self.zoom, self.sort, self.reverse));
        save_folders(&self.folders);
    }
}

fn folders_file() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| theme::home().join(".local/state"))
        .join("nirufm/folders.tsv")
}

fn sort_name(k: SortKey) -> &'static str {
    match k {
        SortKey::Name => "name",
        SortKey::Size => "size",
        SortKey::Modified => "modified",
        SortKey::Type => "type",
    }
}

fn load_folders() -> std::collections::HashMap<PathBuf, (View, usize, SortKey, bool)> {
    let text = std::fs::read_to_string(folders_file()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let path = PathBuf::from(f.next()?);
            let view = View::from_name(f.next()?)?;
            let zoom = f.next()?.parse().ok()?;
            let sort = match f.next()? {
                "size" => SortKey::Size,
                "modified" => SortKey::Modified,
                "type" => SortKey::Type,
                _ => SortKey::Name,
            };
            let reverse = f.next()? == "1";
            Some((path, (view, zoom, sort, reverse)))
        })
        .collect()
}

fn save_folders(map: &std::collections::HashMap<PathBuf, (View, usize, SortKey, bool)>) {
    let file = folders_file();
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut lines: Vec<String> = map
        .iter()
        .filter(|(p, _)| !p.to_string_lossy().contains(['\t', '\n']))
        .map(|(p, (v, z, s, r))| {
            format!(
                "{}\t{}\t{z}\t{}\t{}",
                p.display(),
                v.name(),
                sort_name(*s),
                u8::from(*r)
            )
        })
        .collect();
    lines.sort();
    let tmp = file.with_extension("tmp");
    if std::fs::write(&tmp, lines.join("\n") + "\n").is_ok() {
        let _ = std::fs::rename(tmp, file);
    }
}

/// Pixel glyphs of the view buttons.
fn view_glyph(cv: &mut Canvas, r: Rect, v: View, col: u32) {
    let (w, h) = (r.w, r.h);
    match v {
        View::Icons => {
            let s = (w - 2) / 3;
            for (cx, cy) in [
                (0, 0),
                (1, 0),
                (2, 0),
                (0, 1),
                (1, 1),
                (2, 1),
                (0, 2),
                (1, 2),
                (2, 2),
            ] {
                cv.fill(Rect::new(r.x + cx * (s + 1), r.y + cy * (s + 1), s, s), col);
            }
        }
        View::Thumbs => {
            let s = (w - 1) / 2;
            for (cx, cy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                cv.fill(Rect::new(r.x + cx * (s + 1), r.y + cy * (s + 1), s, s), col);
            }
        }
        View::Compact => {
            let rows = 4;
            let gap = h / rows;
            for k in 0..rows {
                let y = r.y + k * gap;
                cv.fill(Rect::new(r.x, y, 2, 2), col);
                cv.fill(Rect::new(r.x + 3, y, w / 2 - 4, 2), col);
                cv.fill(Rect::new(r.x + w / 2 + 1, y, 2, 2), col);
                cv.fill(Rect::new(r.x + w / 2 + 4, y, w / 2 - 4, 2), col);
            }
        }
        View::List => {
            let rows = 4;
            let gap = h / rows;
            for k in 0..rows {
                let y = r.y + k * gap;
                cv.fill(Rect::new(r.x, y, 2, 2), col);
                cv.fill(Rect::new(r.x + 3, y, w - 3, 2), col);
            }
        }
    }
}

impl App {
    /// Open the gallery on the photos and films of the folder, at `start`
    /// (or the cursor, or the first one).
    fn open_gallery(&mut self, start: Option<PathBuf>) {
        let t = self.tab();
        let items: Vec<Entry> = t
            .shown
            .iter()
            .map(|k| t.all[*k].clone())
            .filter(|e| !e.is_dir && (e.mime.starts_with("image/") || e.mime.starts_with("video/")))
            .collect();
        if items.is_empty() {
            self.flash("No photos or films here".into());
            return;
        }
        let want = start.or_else(|| t.entry(t.cursor).map(|e| e.path.clone()));
        let pos = want
            .and_then(|w| items.iter().position(|e| e.path == w))
            .unwrap_or(0);
        let strip = items.iter().map(|e| (e.path.clone(), None)).collect();
        self.gallery = Some(Gallery {
            items,
            pos,
            images: std::collections::HashMap::new(),
            fitted: None,
            strip,
            exif: std::collections::HashMap::new(),
            slideshow: None,
            zoom: 1.0,
            pan: (0, 0),
            rot: 0,
            info: false,
            full: false,
            grab: None,
            last_click: None,
        });
        self.gallery_load();
        self.dirty = true;
    }

    fn close_gallery(&mut self) {
        if let Some(g) = self.gallery.take() {
            if g.full {
                self.x.toggle_fullscreen(false);
            }
            // The browser shows where the gallery ended.
            let here = g.items[g.pos].path.clone();
            if let Some(k) = self
                .tab()
                .shown
                .iter()
                .position(|k| self.tab().all[*k].path == here)
            {
                self.set_cursor(k, false, false);
            }
        }
        self.dirty = true;
    }

    /// Load the current photo and its neighbours; forget the far ones.
    fn gallery_load(&mut self) {
        let max = (self.cv.w.max(self.cv.h) as u32).clamp(800, 2560);
        let types = self.thumb_types.clone();
        let Some(g) = self.gallery.as_mut() else {
            return;
        };
        let n = g.items.len();
        let near: Vec<usize> = [0isize, 1, -1, 2]
            .iter()
            .map(|d| ((g.pos as isize + d).rem_euclid(n as isize)) as usize)
            .collect();
        let keep: Vec<PathBuf> = near.iter().map(|i| g.items[*i].path.clone()).collect();
        g.images.retain(|p, _| keep.contains(p));
        for i in near {
            let e = g.items[i].clone();
            if g.images.contains_key(&e.path) {
                continue;
            }
            g.images.insert(e.path.clone(), None);
            let tx = self.gallery_tx.clone();
            let wake = self.wake;
            let types = types.clone();
            std::thread::spawn(move || {
                let pv = make_preview(&e, &types, max);
                let _ = tx.send((e.path.clone(), pv.image));
                crate::ops::poke(wake);
            });
        }
        // Thumbnails for the film strip around the current photo.
        let lo = g.pos.saturating_sub(12);
        let hi = (g.pos + 13).min(n);
        let want: Vec<Entry> = g.items[lo..hi].to_vec();
        for e in want {
            let missing = g.strip.get(&e.path).is_some_and(|v| v.is_none());
            if missing && crate::thumbs::wanted(&e.mime, &self.thumb_types) {
                self.thumbs.request(crate::thumbs::Request {
                    path: e.path.clone(),
                    mime: e.mime.clone(),
                    mtime: e.mtime,
                    size: 128,
                });
            }
        }
        let cur = g.items[g.pos].path.clone();
        if !g.exif.contains_key(&cur) {
            g.exif.insert(cur.clone(), crate::exif::read(&cur));
        }
    }

    fn gallery_step(&mut self, d: isize, slideshow: bool) {
        let Some(g) = self.gallery.as_mut() else {
            return;
        };
        let n = g.items.len() as isize;
        g.pos = ((g.pos as isize + d).rem_euclid(n)) as usize;
        g.zoom = 1.0;
        g.pan = (0, 0);
        g.rot = 0;
        if slideshow {
            g.slideshow = Some(Instant::now());
        }
        self.gallery_load();
        self.dirty = true;
    }

    fn gallery_key(&mut self, sym: u32, ctrl: bool) {
        let Some(g) = self.gallery.as_mut() else {
            return;
        };
        let cur = g.items[g.pos].clone();
        match sym {
            0xff1b => {
                if g.full {
                    g.full = false;
                    self.x.toggle_fullscreen(false);
                } else {
                    self.close_gallery();
                }
            }
            0x71 => self.close_gallery(), // q
            0xff53 | 0xff56 | 0xff54 | 0x6c => self.gallery_step(1, false),
            0xff51 | 0xff55 | 0xff52 | 0x68 | 0xff08 => self.gallery_step(-1, false),
            0xff50 => {
                let p = g.pos as isize;
                self.gallery_step(-p, false)
            }
            0xff57 => {
                let d = g.items.len() as isize - 1 - g.pos as isize;
                self.gallery_step(d, false)
            }
            0x20 => {
                g.slideshow = if g.slideshow.is_some() {
                    None
                } else {
                    Some(Instant::now())
                };
                let on = g.slideshow.is_some();
                self.flash(if on {
                    "Slideshow".into()
                } else {
                    "Slideshow stopped".into()
                });
            }
            0x66 | 0xffc8 => {
                // f, F11
                g.full = !g.full;
                let on = g.full;
                self.x.toggle_fullscreen(on);
            }
            0x2b | 0x3d | 0xffab => g.zoom = (g.zoom * 1.25).min(16.0),
            0x2d | 0xffad => g.zoom = (g.zoom / 1.25).max(1.0),
            0x30 => {
                g.zoom = 1.0;
                g.pan = (0, 0);
            }
            0x31 => g.zoom = 100.0, // 1: actual pixels (clamped when drawn)
            0x72 => g.rot = (g.rot + 1) % 4, // r
            0x52 => g.rot = (g.rot + 3) % 4, // R
            0x69 => g.info = !g.info, // i
            0xff0d | 0xff8d => {
                spawn("xdg-open", &[cur.path.as_os_str()], None);
                self.flash(format!("Opening {}", cur.label));
            }
            0x77 => {
                // w: wallpaper through NIRUWM.
                spawn(
                    "niructl",
                    &[
                        std::ffi::OsStr::new("wallpaper"),
                        std::ffi::OsStr::new("set"),
                        cur.path.as_os_str(),
                    ],
                    None,
                );
                self.flash(format!("Wallpaper: {}", cur.label));
            }
            0x63 if ctrl => {
                self.clip = Some((vec![cur.path.clone()], false));
                self.clip_text = None;
                let _ = self.x.own_clipboard();
                self.flash(format!("Copied {}", cur.label));
            }
            0xffff => {
                // Delete: to the trash, and on to the next one.
                self.submit(crate::ops::Job::Trash {
                    paths: vec![cur.path.clone()],
                });
                let g = self.gallery.as_mut().unwrap();
                g.items.remove(g.pos);
                g.images.remove(&cur.path);
                if g.items.is_empty() {
                    self.gallery = None;
                } else {
                    if g.pos >= g.items.len() {
                        g.pos = 0;
                    }
                    self.gallery_load();
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn gallery_button(&mut self, b: u8, x: i32, y: i32) {
        match b {
            4 => return self.gallery_step(-1, false),
            5 => return self.gallery_step(1, false),
            8 => return self.gallery_step(-1, false),
            9 => return self.gallery_step(1, false),
            3 => return self.close_gallery(),
            _ => {}
        }
        if b != 1 {
            return;
        }
        if let Some(Hit::Item(i)) = self.hit(x, y) {
            // A film strip picture.
            let Some(g) = self.gallery.as_ref() else {
                return;
            };
            let d = i as isize - g.pos as isize;
            return self.gallery_step(d, false);
        }
        let Some(g) = self.gallery.as_mut() else {
            return;
        };
        let double = g.last_click.is_some_and(|t| t.elapsed() < DOUBLE_CLICK);
        g.last_click = Some(Instant::now());
        if double {
            g.full = !g.full;
            let on = g.full;
            self.x.toggle_fullscreen(on);
        } else {
            g.grab = Some((x, y, g.pan.0, g.pan.1));
        }
        self.dirty = true;
    }

    fn draw_gallery(&mut self) {
        let p = self.p;
        let (font, small, pad) = (self.m.font, self.m.small, self.m.pad);
        let (w, h) = (self.cv.w, self.cv.h);
        self.hits.clear();
        self.cv.reset_clip();
        let dark = blend(p.background, 0x000000, 0.6);
        self.cv.fill(Rect::new(0, 0, w, h), dark);
        let full = self.gallery.as_ref().is_some_and(|g| g.full);
        let strip_h = if full { 0 } else { (84.0 * self.m.s) as i32 };
        let top_h = if full { 0 } else { font.height + 2 * pad };
        let area = Rect::new(0, top_h, w, h - top_h - strip_h);
        let g = self.gallery.as_mut().unwrap();
        let cur = g.items[g.pos].clone();
        let img = g.images.get(&cur.path).cloned().flatten();
        let (zoom, pan, rot) = (g.zoom, g.pan, g.rot);
        if let Some(full_img) = img {
            // Rotated once per turn; the fitted copy is kept for redraws.
            let (iw, ih) = if rot % 2 == 1 {
                (full_img.h, full_img.w)
            } else {
                (full_img.w, full_img.h)
            };
            let fit = (area.w as f32 / iw as f32)
                .min(area.h as f32 / ih as f32)
                .min(1.0);
            let scale = (fit * zoom).min(4.0f32.max(fit));
            let dw = (iw as f32 * scale).round() as i32;
            let dh = (ih as f32 * scale).round() as i32;
            let dx = area.x + (area.w - dw) / 2 + pan.0;
            let dy = area.y + (area.h - dh) / 2 + pan.1;
            self.cv.set_clip(area);
            if zoom <= 1.0 {
                let key_ok = g.fitted.as_ref().is_some_and(|(pp, r, fw, fh, _)| {
                    pp == &cur.path && *r == rot && *fw == dw && *fh == dh
                });
                if !key_ok {
                    let turned = crate::icons::rotate(&full_img, rot);
                    let small_img = crate::icons::scale_to_fit(&turned, dw, dh);
                    g.fitted = Some((cur.path.clone(), rot, dw, dh, std::rc::Rc::new(small_img)));
                }
                let f = g.fitted.as_ref().unwrap().4.clone();
                self.cv
                    .image(dx + (dw - f.w) / 2, dy + (dh - f.h) / 2, &f, 255);
            } else {
                let turned = if rot == 0 {
                    full_img.clone()
                } else {
                    std::rc::Rc::new(crate::icons::rotate(&full_img, rot))
                };
                self.cv.image_scaled(&turned, Rect::new(dx, dy, dw, dh));
            }
            self.cv.reset_clip();
            if cur.mime.starts_with("video/") {
                // A big pixel play mark over a film.
                let s = (area.h / 8).max(24);
                let (cx, cy) = (area.x + area.w / 2, area.y + area.h / 2);
                for k in 0..s {
                    let half = (s - k) / 2;
                    self.cv.tint(
                        Rect::new(cx - s / 3 + k, cy - half, 1, 2 * half),
                        0xffffff,
                        200,
                    );
                }
            }
        } else {
            let label = if g.images.get(&cur.path).is_some_and(|v| v.is_none()) {
                "LOADING"
            } else {
                "NO PREVIEW"
            };
            let tw = crate::logo::word_width(label, 3);
            crate::logo::word(
                &mut self.cv,
                area.x + (area.w - tw) / 2,
                area.y + area.h / 2,
                label,
                3,
                p.muted,
                None,
            );
        }
        let g = self.gallery.as_ref().unwrap();
        let (pos, n, info, slideshow) = (g.pos, g.items.len(), g.info, g.slideshow.is_some());
        let exif = g.exif.get(&cur.path).cloned().flatten();
        if !full {
            // Title row: name, position, zoom, keys.
            self.cv.fill(Rect::new(0, 0, w, top_h), p.surface);
            self.cv.fill(Rect::new(0, top_h - 1, w, 1), p.border);
            let lp = (2.0 * self.m.s).round().max(2.0) as i32;
            crate::logo::word(
                &mut self.cv,
                pad * 2,
                (top_h - 7 * lp) / 2,
                "GALLERY",
                lp,
                p.accent,
                None,
            );
            let gx = pad * 3 + crate::logo::word_width("GALLERY", lp);
            let mut title = format!("{}   {}/{}", cur.label, pos + 1, n);
            if zoom > 1.0 {
                title.push_str(&format!("   {:.0}%", zoom * 100.0));
            }
            if slideshow {
                title.push_str("   ▶ slideshow");
            }
            let title = self.text.ellipsize(&title, font, w / 2);
            self.text.draw(
                &mut self.cv,
                gx + pad,
                (top_h - font.height) / 2,
                &title,
                font,
                p.foreground,
            );
            let keys = "←→ browse · Space slideshow · F full · +/- zoom · R turn · I info · W wallpaper · Del trash · Esc back";
            let keys = self.text.ellipsize(keys, small, w / 2 - 3 * pad);
            let kw = self.text.width(&keys, small);
            self.text.draw(
                &mut self.cv,
                w - kw - 2 * pad,
                (top_h - small.height) / 2,
                &keys,
                small,
                p.muted,
            );
            // Film strip.
            let sy = h - strip_h;
            self.cv.fill(Rect::new(0, sy, w, strip_h), p.surface);
            self.cv.fill(Rect::new(0, sy, w, 1), p.border);
            let th = strip_h - 2 * pad;
            let tw = th * 3 / 2;
            let step = tw + pad;
            let visible = (w / step).max(1) as isize;
            let first = (pos as isize - visible / 2)
                .max(0)
                .min((n as isize - visible).max(0)) as usize;
            let items: Vec<Entry> = g.items[first..(first + visible as usize).min(n)].to_vec();
            for (k, e) in items.iter().enumerate() {
                let idx = first + k;
                let r = Rect::new(pad + k as i32 * step, sy + pad, tw, th);
                let thumb = self
                    .gallery
                    .as_ref()
                    .unwrap()
                    .strip
                    .get(&e.path)
                    .cloned()
                    .flatten();
                self.cv.fill(r, p.background);
                if let Some(t) = thumb {
                    self.cv.image(
                        r.x + (r.w - t.w) / 2,
                        r.y + (r.h - t.h) / 2,
                        &t,
                        if idx == pos { 255 } else { 170 },
                    );
                } else if let Some(icon) = self.icon_for(e, th / 2) {
                    self.cv.image(
                        r.x + (r.w - icon.w) / 2,
                        r.y + (r.h - icon.h) / 2,
                        &icon,
                        200,
                    );
                }
                if idx == pos {
                    for d in 0..(2.0 * self.m.s).max(2.0) as i32 {
                        self.cv.frame(r.inset(-d - 1), p.accent);
                    }
                }
                self.hits.push((r, Hit::Item(idx)));
            }
        }
        if info {
            let mut lines = vec![
                cur.label.clone(),
                format!(
                    "{} · {}",
                    dir::human_size(cur.size),
                    dir::human_time(cur.mtime)
                ),
            ];
            if let Some(x) = &exif {
                let l = x.line();
                if !l.is_empty() {
                    lines.push(l);
                }
                if let (Some(a), Some(b)) = (x.width, x.height) {
                    lines.push(format!("{a} × {b} px"));
                }
            }
            let bw = lines
                .iter()
                .map(|l| self.text.width(l, small))
                .max()
                .unwrap_or(0)
                + 4 * pad;
            let bh = lines.len() as i32 * small.height + 2 * pad;
            let r = Rect::new(
                2 * pad,
                area.y + area.h - bh - 2 * pad,
                bw.min(w - 4 * pad),
                bh,
            );
            self.cv.tint(r, p.background, 210);
            self.cv.frame(r, p.border);
            for (k, l) in lines.iter().enumerate() {
                let col = if k == 0 { p.foreground } else { p.muted };
                self.text.draw(
                    &mut self.cv,
                    r.x + 2 * pad,
                    r.y + pad + k as i32 * small.height,
                    l,
                    small,
                    col,
                );
            }
        }
        if let Some((m, _)) = self.message.clone() {
            let mw = self.text.width(&m, small) + 4 * pad;
            let r = Rect::new((w - mw) / 2, area.y + 2 * pad, mw, small.height + pad);
            self.cv.tint(r, p.background, 220);
            self.text.draw(
                &mut self.cv,
                r.x + 2 * pad,
                r.y + pad / 2,
                &m,
                small,
                p.accent,
            );
        }
    }
}

impl App {
    fn search_key(&mut self, sym: u32, ctrl: bool, shift: bool) {
        let ch = keysym_char(sym);
        let Some(ed) = self.search_edit.as_mut() else {
            return;
        };
        self.dirty = true;
        // Down moves into the results, as Enter does.
        let key = if sym == 0xff54 || sym == 0xff99 {
            EditKey::Commit
        } else {
            let before = ed.text.clone();
            let k = ed.key(sym, ctrl, shift, ch);
            if ed.text != before {
                self.search_due = Some(Instant::now() + Duration::from_millis(180));
            }
            k
        };
        match key {
            EditKey::Cancel => {
                // Esc: back to the folder.
                self.search_edit = None;
                self.search_due = None;
                self.search_gen
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.searching = false;
                let a = self.active;
                self.load_tab(a, true);
            }
            EditKey::Commit => {
                // Keep the results, move into them; a search still due
                // for the last keys starts now.
                let q = self.search_edit.take().map(|e| e.text).unwrap_or_default();
                if self.search_due.take().is_some() {
                    self.start_search(q);
                }
            }
            EditKey::Copy => {
                let s = ed.selected().to_string();
                self.copy_text(s);
            }
            EditKey::Cut => {
                if let Some(s) = ed.cut() {
                    self.copy_text(s);
                    self.search_due = Some(Instant::now() + Duration::from_millis(180));
                }
            }
            EditKey::Paste => self.paste_text(),
            EditKey::Next(_) | EditKey::Edited | EditKey::Ignored => {}
        }
    }

    /// Search names below the folder on a thread; results stream in.
    fn start_search(&mut self, q: String) {
        let gen = self
            .search_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let words: Vec<String> = q
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let t = self.tab_mut();
        t.all.clear();
        t.shown.clear();
        t.selected.clear();
        t.cursor = 0;
        t.anchor = 0;
        t.scroll = 0;
        t.error = None;
        if words.is_empty() {
            t.search = None;
            self.searching = false;
            let a = self.active;
            self.load_tab(a, true);
            return;
        }
        t.search = Some(q);
        let root = t.path.clone();
        self.searching = true;
        let tx = self.search_tx.clone();
        let wake = self.wake;
        let current = self.search_gen.clone();
        let hidden = self.hidden;
        let mime = std::sync::Arc::new(Mime::load());
        std::thread::spawn(move || {
            let alive = || current.load(std::sync::atomic::Ordering::Relaxed) == gen;
            let mut stack = vec![root.clone()];
            let mut batch = Vec::new();
            let mut last = Instant::now();
            let mut found = 0usize;
            while let Some(dir) = stack.pop() {
                if !alive() {
                    return;
                }
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !hidden && name.starts_with('.') {
                        continue;
                    }
                    let Ok(ft) = e.file_type() else { continue };
                    let p = e.path();
                    if ft.is_dir() && !ft.is_symlink() {
                        stack.push(p.clone());
                    }
                    let lower = name.to_lowercase();
                    if words.iter().all(|w| lower.contains(w)) {
                        if let Ok(mut v) = crate::dir::read_one(&p, &mime) {
                            v.label = p
                                .strip_prefix(&root)
                                .map(|r| r.display().to_string())
                                .unwrap_or(name);
                            batch.push(v);
                            found += 1;
                        }
                    }
                }
                if !batch.is_empty()
                    && (batch.len() >= 200 || last.elapsed() > Duration::from_millis(120))
                {
                    let _ = tx.send((gen, std::mem::take(&mut batch), false));
                    crate::ops::poke(wake);
                    last = Instant::now();
                }
                if found >= 20_000 {
                    break;
                }
            }
            let _ = tx.send((gen, batch, true));
            crate::ops::poke(wake);
        });
        self.dirty = true;
    }
}

impl App {
    fn open_props(&mut self) {
        let paths = self.selection_paths();
        let paths = if paths.is_empty() {
            vec![self.tab().path.clone()]
        } else {
            paths
        };
        let totals = std::sync::Arc::new(std::sync::Mutex::new((0u64, 0u64, 0u64, false)));
        let (t, list, wake) = (totals.clone(), paths.clone(), self.wake);
        std::thread::spawn(move || {
            let mut last = Instant::now();
            let mut stack: Vec<PathBuf> = list;
            while let Some(p) = stack.pop() {
                let Ok(m) = std::fs::symlink_metadata(&p) else {
                    continue;
                };
                if let Ok(mut g) = t.lock() {
                    if m.is_dir() {
                        g.2 += 1;
                    } else {
                        g.0 += m.len();
                        g.1 += 1;
                    }
                }
                if m.is_dir() {
                    if let Ok(rd) = std::fs::read_dir(&p) {
                        stack.extend(rd.flatten().map(|e| e.path()));
                    }
                }
                if last.elapsed() > Duration::from_millis(150) {
                    last = Instant::now();
                    crate::ops::poke(wake);
                }
            }
            if let Ok(mut g) = t.lock() {
                g.3 = true;
            }
            crate::ops::poke(wake);
        });
        self.props = Some(Props {
            paths,
            totals,
            sha: std::sync::Arc::new(std::sync::Mutex::new(None)),
        });
        self.dirty = true;
    }

    fn props_checksum(&mut self) {
        let Some(pr) = self.props.as_ref() else {
            return;
        };
        if pr.paths.len() != 1 || pr.paths[0].is_dir() {
            return;
        }
        let (path, sha, wake) = (pr.paths[0].clone(), pr.sha.clone(), self.wake);
        if let Ok(mut g) = sha.lock() {
            *g = Some("computing…".into());
        }
        std::thread::spawn(move || {
            let out = std::process::Command::new("sha256sum").arg(&path).output();
            let v = match out {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string(),
                _ => "sha256sum failed".into(),
            };
            if let Ok(mut g) = sha.lock() {
                *g = Some(v);
            }
            crate::ops::poke(wake);
        });
    }

    fn draw_props(&mut self) {
        let Some(pr) = self.props.as_ref() else {
            return;
        };
        let p = self.p;
        let (font, small, pad) = (self.m.font, self.m.small, self.m.pad);
        let paths = pr.paths.clone();
        let (bytes, files, dirs, done) = pr.totals.lock().map(|g| *g).unwrap_or_default();
        let sha = pr.sha.lock().ok().and_then(|g| g.clone());
        let mut rows: Vec<(String, String)> = Vec::new();
        let title;
        if paths.len() == 1 {
            use std::os::unix::fs::MetadataExt;
            let path = &paths[0];
            title = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let lm = std::fs::symlink_metadata(path).ok();
            let m = std::fs::metadata(path).ok().or(lm.clone());
            let is_dir = m.as_ref().is_some_and(|m| m.is_dir());
            let mime = if is_dir {
                "folder".to_string()
            } else {
                self.mime.for_name(&title).to_string()
            };
            rows.push(("Type".into(), mime));
            rows.push((
                "Location".into(),
                path.parent().map(places::pretty).unwrap_or_default(),
            ));
            if lm.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
                rows.push((
                    "Link to".into(),
                    std::fs::read_link(path)
                        .map(|t| t.display().to_string())
                        .unwrap_or_default(),
                ));
            }
            let size = if is_dir {
                format!(
                    "{} in {files} files, {} folders{}",
                    dir::human_size(bytes),
                    dirs.saturating_sub(1),
                    if done { "" } else { " (counting…)" }
                )
            } else {
                format!("{} ({bytes} bytes)", dir::human_size(bytes))
            };
            rows.push(("Size".into(), size));
            if let Some(m) = &m {
                rows.push(("Modified".into(), dir::human_time(m.mtime())));
                rows.push(("Accessed".into(), dir::human_time(m.atime())));
                let mode = m.mode() & 0o7777;
                rows.push((
                    "Permissions".into(),
                    format!("{} ({mode:o})", perm_string(mode, is_dir)),
                ));
                rows.push((
                    "Owner".into(),
                    format!("{}:{}", user_name(m.uid()), group_name(m.gid())),
                ));
            }
            if let Some((free, total)) = dir::space(path) {
                rows.push((
                    "Disk".into(),
                    format!(
                        "{} free of {}",
                        dir::human_size(free),
                        dir::human_size(total)
                    ),
                ));
            }
            if !is_dir {
                rows.push((
                    "SHA-256".into(),
                    sha.unwrap_or_else(|| "press S to compute".into()),
                ));
            }
        } else {
            title = format!("{} items", paths.len());
            rows.push((
                "Size".into(),
                format!(
                    "{} in {files} files, {dirs} folders{}",
                    dir::human_size(bytes),
                    if done { "" } else { " (counting…)" }
                ),
            ));
        }
        let label_w = rows
            .iter()
            .map(|(k, _)| self.text.width(k, small))
            .max()
            .unwrap_or(0)
            + 2 * pad;
        let val_w = rows
            .iter()
            .map(|(_, v)| self.text.width(v, small))
            .max()
            .unwrap_or(0);
        let w = (label_w + val_w + 6 * pad)
            .max((360.0 * self.m.s) as i32)
            .min(self.cv.w - 4 * pad);
        let row_h = small.height + pad / 2;
        let h = 4 * pad + font.height + rows.len() as i32 * row_h + small.height + 2 * pad;
        let r = Rect::new((self.cv.w - w) / 2, (self.cv.h - h) / 3, w, h);
        self.cv
            .tint(Rect::new(0, 0, self.cv.w, self.cv.h), p.background, 140);
        let sh = (4.0 * self.m.s) as i32;
        self.cv
            .tint(Rect::new(r.x + sh, r.y + sh, r.w, r.h), 0x000000, 120);
        self.cv.fill(r, p.surface);
        self.cv.frame(r, p.accent);
        let t = self.text.ellipsize(&title, font, w - 4 * pad);
        self.text.draw(
            &mut self.cv,
            r.x + 2 * pad,
            r.y + 2 * pad,
            &t,
            font,
            p.foreground,
        );
        let mut y = r.y + 3 * pad + font.height;
        for (k, v) in rows {
            self.text
                .draw(&mut self.cv, r.x + 2 * pad, y, &k, small, p.muted);
            let v = self.text.ellipsize(&v, small, w - label_w - 4 * pad);
            self.text.draw(
                &mut self.cv,
                r.x + 2 * pad + label_w,
                y,
                &v,
                small,
                p.foreground,
            );
            y += row_h;
        }
        self.text.draw(
            &mut self.cv,
            r.x + 2 * pad,
            y + pad / 2,
            "S checksum · any key closes",
            small,
            p.muted,
        );
    }
}

/// A password as it is shown: one dot for each character.
fn mask(s: &str) -> String {
    "•".repeat(s.chars().count())
}

/// The offset in `s` of offset `at` in its mask.
fn unmask(s: &str, at: usize) -> usize {
    let n = at / '•'.len_utf8();
    s.char_indices().nth(n).map_or(s.len(), |(i, _)| i)
}

/// An editor shown as its mask, with the cursor and selection in place.
fn masked(ed: &crate::edit::LineEdit) -> crate::edit::LineEdit {
    let dots = |i: usize| ed.text[..i].chars().count() * '•'.len_utf8();
    crate::edit::LineEdit::selecting(&mask(&ed.text), dots(ed.anchor), dots(ed.cursor))
}

fn perm_string(mode: u32, dir: bool) -> String {
    let mut s = String::from(if dir { "d" } else { "-" });
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    s
}

fn user_name(uid: u32) -> String {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return uid.to_string();
    }
    unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }
        .to_string_lossy()
        .into_owned()
}

fn group_name(gid: u32) -> String {
    let gr = unsafe { libc::getgrgid(gid) };
    if gr.is_null() {
        return gid.to_string();
    }
    unsafe { std::ffi::CStr::from_ptr((*gr).gr_name) }
        .to_string_lossy()
        .into_owned()
}

impl App {
    /// Keys of the dialog for a name that exists: the first letter of each
    /// choice, A for "apply to all", Enter keeps both, Esc stops the job.
    fn ask_key(&mut self, sym: u32) {
        use crate::ops::Choice;
        let folders = self
            .ask
            .as_ref()
            .is_some_and(|a| a.existing.is_dir && a.incoming.is_dir);
        match sym | 0x20 {
            0x72 => self.answer(Choice::Replace),          // r
            0x73 => self.answer(Choice::Skip),             // s
            0x6b => self.answer(Choice::KeepBoth),         // k
            0x6d if folders => self.answer(Choice::Merge), // m
            0x61 => {
                // a
                if let Some(a) = self.ask.as_mut() {
                    a.all = !a.all;
                }
            }
            _ => match sym {
                0xff0d | 0xff8d => self.answer(Choice::KeepBoth),
                0xff1b => {
                    // Stop the job: what is done stays done (and undoable).
                    if let Some((_, cancel)) = self.jobs.front() {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    self.answer(Choice::Skip);
                    self.flash("Cancelling…".into());
                }
                _ => {}
            },
        }
        self.dirty = true;
    }

    fn answer(&mut self, choice: crate::ops::Choice) {
        let Some(a) = self.ask.take() else {
            return;
        };
        let _ = self
            .worker
            .answer
            .send(crate::ops::Answer { choice, all: a.all });
        self.dirty = true;
    }

    /// The dialog for a name that exists where an item is pasted, moved or
    /// dropped.
    fn draw_ask(&mut self) {
        let Some(a) = self.ask.as_ref() else {
            return;
        };
        use crate::ops::Choice;
        let p = self.p;
        let (font, small, pad) = (self.m.font, self.m.small, self.m.pad);
        let folders = a.existing.is_dir && a.incoming.is_dir;
        let what = if a.existing.is_dir {
            "A folder"
        } else {
            "A file"
        };
        let lines = [
            format!(
                "{what} named \"{}\" is already in {}.",
                a.name,
                places::pretty(&a.folder)
            ),
            format!("Existing: {}", side_text(&a.existing)),
            format!("New:      {}", side_text(&a.incoming)),
        ];
        let all = a.all;
        let mut buttons = vec![(Choice::Replace, "R  Replace")];
        if folders {
            buttons.push((Choice::Merge, "M  Merge"));
        }
        buttons.push((Choice::KeepBoth, "K  Keep both"));
        buttons.push((Choice::Skip, "S  Skip"));
        let w = (self.cv.w * 2 / 3)
            .clamp((420.0 * self.m.s) as i32, (680.0 * self.m.s) as i32)
            .min(self.cv.w - 2 * pad);
        let bh = font.height + pad;
        let h = 4 * pad
            + font.height
            + 3 * (small.height + pad / 3)
            + 2 * pad
            + bh
            + 2 * (small.height + pad / 2);
        let r = Rect::new((self.cv.w - w) / 2, ((self.cv.h - h) / 3).max(0), w, h);
        self.cv
            .tint(Rect::new(0, 0, self.cv.w, self.cv.h), p.background, 140);
        let sh = (4.0 * self.m.s) as i32;
        self.cv
            .tint(Rect::new(r.x + sh, r.y + sh, r.w, r.h), 0x000000, 120);
        self.cv.fill(r, p.surface);
        self.cv.frame(r, p.warning);
        // Everything outside the dialog is inert while it is open.
        self.hits
            .push((Rect::new(0, 0, self.cv.w, self.cv.h), Hit::MenuOutside));
        let mut y = r.y + 2 * pad;
        crate::logo::word(
            &mut self.cv,
            r.x + 2 * pad,
            y,
            "NAME EXISTS",
            (2.0 * self.m.s).round().max(2.0) as i32,
            p.warning,
            None,
        );
        y += font.height + pad;
        for (k, line) in lines.iter().enumerate() {
            let line = self.text.ellipsize(line, small, r.w - 4 * pad);
            let col = if k == 0 { p.foreground } else { p.muted };
            self.text
                .draw(&mut self.cv, r.x + 2 * pad, y, &line, small, col);
            y += small.height + pad / 3;
        }
        y += pad;
        let mut x = r.x + 2 * pad;
        for (c, label) in buttons {
            let bw = self.text.width(label, font) + 2 * pad;
            let br = Rect::new(x, y, bw, bh);
            self.cv.fill(br, p.background);
            self.cv.frame(
                br,
                if c == Choice::KeepBoth {
                    p.accent
                } else {
                    p.border
                },
            );
            self.text.draw(
                &mut self.cv,
                br.x + pad,
                br.y + pad / 2,
                label,
                font,
                p.foreground,
            );
            self.hits.push((br, Hit::Answer(c)));
            x += bw + pad;
        }
        y += bh + pad;
        let mark = if all { "[x]" } else { "[ ]" };
        let label = format!("{mark} A  Apply to all remaining conflicts");
        let tw = self
            .text
            .draw(&mut self.cv, r.x + 2 * pad, y, &label, small, p.foreground);
        self.hits.push((
            Rect::new(r.x + 2 * pad, y, tw, small.height),
            Hit::AnswerAll,
        ));
        y += small.height + pad / 2;
        let hint = if folders {
            "Merge puts the contents together and asks about names inside · Enter keep both · Esc stop"
        } else {
            "Replace moves the existing one to the trash · Enter keep both · Esc stop"
        };
        let hint = self.text.ellipsize(hint, small, r.w - 4 * pad);
        self.text
            .draw(&mut self.cv, r.x + 2 * pad, y, &hint, small, p.muted);
    }
}

/// "12 kB, changed 2026-11-20 14:02" for the dialog of a name that exists.
fn side_text(s: &crate::ops::Side) -> String {
    let what = if s.is_dir {
        "folder".to_string()
    } else {
        dir::human_size(s.size)
    };
    format!("{what}, changed {}", dir::human_time(s.mtime))
}

impl App {
    /// Online NIRUSHARE devices, asked for in the background.
    fn refresh_share_hosts(&self) {
        if !crate::archive::have("nirushare") {
            return;
        }
        let hosts = self.share_hosts.clone();
        std::thread::spawn(move || {
            let Ok(out) = std::process::Command::new("nirushare")
                .arg("devices")
                .stderr(std::process::Stdio::null())
                .output()
            else {
                return;
            };
            let text = String::from_utf8_lossy(&out.stdout);
            // A JSON list of {"host": "...", "online": true}; read without a parser.
            let mut v = Vec::new();
            for obj in text.split('}') {
                let host = obj
                    .split("\"host\"")
                    .nth(1)
                    .and_then(|r| r.split('"').nth(1))
                    .map(str::to_string);
                let online = obj.contains("\"online\": true") || obj.contains("\"online\":true");
                if let (Some(h), true) = (host, online) {
                    v.push(h);
                }
            }
            if let Ok(mut g) = hosts.lock() {
                *g = v;
            }
        });
    }

    /// Ask the Nextcloud client how each shown item is synced.
    fn nextcloud_status(&mut self) {
        self.nc_status.clear();
        let dir = self.tab().path.clone();
        if !crate::nextcloud::in_sync_folder(&dir) {
            return;
        }
        let paths: Vec<PathBuf> = self
            .tab()
            .shown
            .iter()
            .take(400)
            .map(|k| self.tab().all[*k].path.clone())
            .collect();
        let (tx, wake) = (self.nc_tx.clone(), self.wake);
        std::thread::spawn(move || {
            for (p, st) in crate::nextcloud::statuses(&paths) {
                let _ = tx.send((p, st));
            }
            crate::ops::poke(wake);
        });
    }

    /// A small pixel badge on an item: synced, syncing or error.
    fn draw_nc_badge(&mut self, path: &Path, x: i32, y: i32) {
        let Some(st) = self.nc_status.get(path).copied() else {
            return;
        };
        let u = (2.0 * self.m.s).max(2.0) as i32;
        let (col, glyph): (u32, &[u8]) = match st {
            1 => (0x6fbf73, &[0b000, 0b001, 0b010, 0b100, 0b000]),
            2 => (0x6fa8dc, &[0b010, 0b111, 0b010, 0b010, 0b010]),
            3 => (0xd77a7a, &[0b101, 0b010, 0b101, 0b000, 0b000]),
            _ => return,
        };
        let r = Rect::new(x, y, 5 * u, 6 * u);
        self.cv.fill(r, self.p.background);
        self.cv.frame(r, col);
        for (row, bits) in glyph.iter().enumerate() {
            for c in 0..3 {
                if bits & (4 >> c) != 0 {
                    self.cv.fill(
                        Rect::new(x + u + c * u, y + u / 2 + row as i32 * u, u, u),
                        col,
                    );
                }
            }
        }
    }

    fn draw_help(&mut self) {
        if !self.help {
            return;
        }
        let p = self.p;
        let (font, small, pad) = (self.m.font, self.m.small, self.m.pad);
        let groups: [(&str, &[(&str, &str)]); 4] = [
            (
                "BROWSE",
                &[
                    ("Enter", "open"),
                    ("Backspace", "parent folder"),
                    ("Alt+Left/Right", "back, forward"),
                    ("Ctrl+L", "type a path"),
                    ("Ctrl+F", "search below"),
                    ("letters", "jump to a name"),
                    ("Ctrl+H", "hidden files"),
                    ("Ctrl+1-4", "icons, thumbnails, compact, list"),
                    ("Ctrl+wheel, Ctrl +/-", "zoom"),
                    ("Space", "preview"),
                    ("Ctrl+G", "gallery"),
                ],
            ),
            (
                "FILES",
                &[
                    ("Ctrl+C, Ctrl+X, Ctrl+V", "copy, cut, paste"),
                    ("Ctrl+Shift+C", "copy the path"),
                    ("Delete", "to the trash"),
                    ("Shift+Delete", "delete for good"),
                    ("Ctrl+Z", "undo"),
                    (
                        "R, K, S, M, A",
                        "name exists: replace, keep both, skip, merge, all",
                    ),
                    ("F2", "rename (several: find, replace, {n})"),
                    ("Ctrl+Shift+N", "new folder"),
                    ("Ctrl+Alt+N", "new empty file (templates: menu)"),
                    ("Alt+Enter", "properties"),
                    ("right-click", "menu: open with, pack, links"),
                    ("drag on blank space", "select (Ctrl adds, Shift flips)"),
                    ("drag files", "move (Ctrl copies, Shift moves)"),
                    ("hold over a folder", "opens it while dragging"),
                ],
            ),
            (
                "WINDOW",
                &[
                    ("Ctrl+T, Ctrl+W", "new tab, close tab"),
                    ("Ctrl+Tab", "next tab"),
                    ("F3", "two panes"),
                    ("Tab", "other pane"),
                    ("F4", "terminal here"),
                    ("F5", "reload"),
                    ("Ctrl+D", "bookmark"),
                    ("Ctrl+K", "connect to a server"),
                    ("Ctrl+Q", "quit"),
                ],
            ),
            (
                "GALLERY",
                &[
                    ("Left, Right", "browse"),
                    ("Space", "slideshow"),
                    ("F", "full screen"),
                    ("+, -, 0", "zoom"),
                    ("R", "turn"),
                    ("I", "info"),
                    ("W", "wallpaper"),
                    ("Delete", "trash"),
                    ("Esc", "back"),
                ],
            ),
        ];
        let col_w = (self.cv.w - 6 * pad) / 2;
        let row = small.height + pad / 3;
        let rows_l = groups[0].1.len() + groups[1].1.len() + 2;
        let rows_r = groups[2].1.len() + groups[3].1.len() + 2;
        let h = (rows_l.max(rows_r) as i32) * row + 4 * pad + 2 * font.height + 2 * pad;
        let w = self.cv.w - 4 * pad;
        let r = Rect::new(2 * pad, (self.cv.h - h).max(0) / 2, w, h.min(self.cv.h));
        self.cv
            .tint(Rect::new(0, 0, self.cv.w, self.cv.h), p.background, 170);
        self.cv.fill(r, p.surface);
        self.cv.frame(r, p.accent);
        let lp = (2.0 * self.m.s).round().max(2.0) as i32;
        crate::logo::word(
            &mut self.cv,
            r.x + 2 * pad,
            r.y + 2 * pad,
            "NIRUFM KEYS",
            lp,
            p.accent,
            None,
        );
        let key_w = (170.0 * self.m.s) as i32;
        for (half, list) in [(0, [0usize, 1]), (1, [2, 3])] {
            let mut y = r.y + 3 * pad + 7 * lp;
            let x = r.x + 2 * pad + half * (col_w + pad);
            for gi in list {
                let (title, keys) = groups[gi];
                self.text.draw(&mut self.cv, x, y, title, small, p.accent);
                y += row;
                for (k, d) in keys.iter() {
                    let kk = self.text.ellipsize(k, small, key_w - pad);
                    self.text.draw(&mut self.cv, x, y, &kk, small, p.foreground);
                    self.text
                        .draw(&mut self.cv, x + key_w, y, d, small, p.muted);
                    y += row;
                }
                y += row / 2;
            }
        }
    }
}

fn mimes_image(mime: &Mime, p: &Path) -> bool {
    let n = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    mime.for_name(&n).starts_with("image/")
}

/// A NIRUX event sound, quiet under Do not disturb.
fn play_sound(event: &'static str) {
    std::thread::spawn(move || {
        let dnd = std::process::Command::new("niructl")
            .args(["dnd", "status"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(" on"))
            .unwrap_or(false);
        if dnd {
            return;
        }
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| theme::home().join(".local/share"));
        let f = data.join("nirux/sounds").join(format!("{event}.ogg"));
        if f.is_file() {
            let _ = std::process::Command::new("pw-play")
                .arg(&f)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    });
}

impl App {
    /// Recently used files as a list, like search results.
    fn show_recent(&mut self) {
        self.search_gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.searching = false;
        let files = places::recent_files(200);
        let entries: Vec<Entry> = files
            .iter()
            .filter_map(|p| dir::read_one(p, &self.mime).ok())
            .collect();
        let home = theme::home();
        let t = self.tab_mut();
        t.path = home;
        t.search = Some(RECENT.into());
        t.all = entries;
        t.shown = (0..t.all.len()).collect();
        t.selected = vec![false; t.all.len()];
        t.cursor = 0;
        t.anchor = 0;
        t.scroll = 0;
        t.error = None;
        if self.view != View::List {
            self.view = View::List;
            self.layout_only();
        }
        self.dirty = true;
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

/// The trash shows full when its files folder has anything in it.
fn trash_has_files(files: &Path) -> bool {
    std::fs::read_dir(files).is_ok_and(|mut d| d.next().is_some())
}

/// Icon names for the XDG folders (the NIRUX set and Papirus have them).
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
/// Tab in the path bar: `typed` with the folder name it starts completed
/// as far as the folders there agree ("/" added when only one fits).
fn complete_folder(typed: &str, hidden: bool) -> Option<String> {
    let expanded = expand(typed);
    let (dir, prefix) = match expanded.rfind('/') {
        Some(i) => (expanded[..=i].to_string(), expanded[i + 1..].to_string()),
        None => ("./".into(), expanded.clone()),
    };
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.starts_with(&prefix) && (hidden || !n.starts_with('.') || prefix.starts_with('.'))
        })
        .collect();
    names.sort_by(|a, b| dir::natural(a, b));
    let first = names.first()?;
    let common: String = names.iter().fold(first.clone(), |acc, n| {
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
    let base = typed.rfind('/').map_or("", |i| &typed[..=i]);
    Some(format!("{base}{done}"))
}

fn keysym_char(sym: u32) -> Option<char> {
    match sym {
        0x20..=0x7e | 0xa0..=0xff => char::from_u32(sym),
        0x0100_0000..=0x0110_ffff => char::from_u32(sym - 0x0100_0000),
        _ => None,
    }
}

/// Start a program detached from this one (its own session, no stdio).
/// The suite's terminal, started in `dir` (every terminal starts in its own
/// working folder, so no terminal-specific option is needed).
fn open_terminal(dir: &Path) {
    let t = crate::theme::configured_terminal(&crate::theme::config_home());
    let args: Vec<&std::ffi::OsStr> = t[1..].iter().map(std::ffi::OsStr::new).collect();
    spawn(&t[0], &args, Some(dir));
}

pub(crate) fn spawn(cmd: &str, args: &[&std::ffi::OsStr], cwd: Option<&Path>) {
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

// ------------------------------------------------------------ rubber band

impl App {
    /// The part of an item's cell its picture and name take up; the rest of
    /// the cell is room to start a band, as between the files.
    fn item_body(&mut self, i: usize) -> Rect {
        let r = self.item_rect(i);
        let Some(label) = self.tab().entry(i).map(|e| e.label.clone()) else {
            return r;
        };
        let (pad, font) = (self.m.pad, self.m.font);
        match self.view {
            View::List => {
                let si = self.m.small_icon;
                let name_w = self.list_name_w();
                let tw = self.text.width(&label, font).min(name_w - si - 2 * pad);
                Rect::new(r.x, r.y, (2 * pad + si + tw + pad).min(r.w), r.h)
            }
            View::Compact => {
                let icon = self.m.small_icon;
                let tw = self.text.width(&label, font).min(r.w - icon - 2 * pad);
                Rect::new(r.x, r.y, (pad + icon + tw + pad).min(r.w), r.h)
            }
            _ => {
                let (cell_w, _, _) = self.grid();
                let pic = self.picture_size().max(self.m.icon);
                let tw = self.text.width(&label, font).min(cell_w - pad);
                let w = (pic.max(tw) + pad).min(r.w);
                Rect::new(r.x + (r.w - w) / 2, r.y + pad / 2, w, r.h - pad)
            }
        }
    }

    /// The width of the Name column of the list view.
    fn list_name_w(&mut self) -> i32 {
        let (pad, small) = (self.m.pad, self.m.small);
        let c = self.content;
        let size_w = self.text.width("0000 MB", small) + 2 * pad;
        let date_w = self.text.width("2026-11-21 18:04", small) + 2 * pad;
        let type_w = (c.w / 5).clamp((90.0 * self.m.s) as i32, (240.0 * self.m.s) as i32);
        c.w - size_w - date_w - type_w - pad
    }

    fn start_band(&mut self, x: i32, y: i32, ctrl: bool, shift: bool) {
        use crate::band::{Band, Mode};
        let mode = if shift {
            Mode::Toggle
        } else if ctrl {
            Mode::Add
        } else {
            Mode::Replace
        };
        let scroll = self.tab().scroll;
        let before = self.tab().selected.clone();
        self.band = Some(Band::new(self.active, (x, y + scroll), before, mode));
        self.pointer = (x, y);
    }

    fn band_motion(&mut self, x: i32, y: i32) {
        self.pointer = (x, y);
        let far = (6.0 * self.m.s) as i32;
        let Some(b) = self.band.as_mut() else { return };
        b.to = (x, y);
        if !b.live {
            let scroll = self.tabs.get(b.tab).map_or(0, |t| t.scroll);
            let (fx, fy) = (b.from.0, b.from.1 - scroll);
            if (x - fx).abs() <= far && (y - fy).abs() <= far {
                return;
            }
            b.live = true;
            self.hover = None;
        }
        self.edge = self.edge_speed_at(y);
        self.band_update();
    }

    /// Select what the band covers now. Only the rows it spans are looked at.
    fn band_update(&mut self) {
        let Some(b) = self.band.as_ref() else { return };
        if !b.live || b.tab != self.active || b.before.len() != self.tab().shown.len() {
            return;
        }
        let scroll = self.tab().scroll;
        let r = b.rect(scroll);
        let n = self.tab().shown.len();
        let c = self.content;
        // The candidates: the items in the rows the band spans.
        let candidates: Vec<usize> = match self.view {
            View::List => {
                let row_h = self.m.row_h;
                crate::band::rows(r.y, r.y + r.h, c.y + row_h, row_h, n)
                    .map(|(a, z)| (a..=z).collect())
                    .unwrap_or_default()
            }
            _ => {
                let (cw, ch, _) = self.grid();
                let cols = self.cols.max(1) as usize;
                let total = n.div_ceil(cols);
                match crate::band::rows(r.y, r.y + r.h, c.y + self.m.pad, ch, total) {
                    Some((a, z)) => {
                        let left = c.x + self.m.pad;
                        let c0 = ((r.x - left).max(0) / cw.max(1)) as usize;
                        let c1 = (((r.x + r.w - left).max(0) / cw.max(1)) as usize).min(cols - 1);
                        (a..=z)
                            .flat_map(|row| (c0..=c1).map(move |col| row * cols + col))
                            .filter(|i| *i < n)
                            .collect()
                    }
                    None => Vec::new(),
                }
            }
        };
        let mut hit = vec![false; n];
        let mut last = None;
        for i in candidates {
            // A list row is taken whole: the band need only cross it.
            let mut body = if self.view == View::List {
                Rect::new(r.x, self.item_rect(i).y, r.w, self.m.row_h)
            } else {
                self.item_body(i)
            };
            body.y += scroll;
            if crate::band::overlaps(&body, &r) {
                hit[i] = true;
                last = Some(i);
            }
        }
        let Some(b) = self.band.as_ref() else { return };
        let new = crate::band::combine(&b.before, &hit, b.mode);
        let t = self.tab_mut();
        if new != t.selected {
            t.selected = new;
        }
        if let Some(i) = last {
            t.cursor = i;
            t.anchor = i;
        }
        // The band itself moved: it is drawn again either way.
        self.dirty = true;
    }

    /// A click on blank space (no band drawn) clears the selection, as before.
    fn band_release(&mut self) {
        let Some(b) = self.band.take() else { return };
        self.edge = 0;
        self.edge_next = None;
        if !b.live && b.mode == crate::band::Mode::Replace {
            self.clear_selection();
        }
        self.dirty = true;
    }

    /// Esc during a band: the selection goes back to what it was.
    fn band_cancel(&mut self) {
        if let Some(b) = self.band.take() {
            let t = self.tab_mut();
            if b.before.len() == t.selected.len() {
                t.selected = b.before;
            }
        }
        self.edge = 0;
        self.edge_next = None;
        self.dirty = true;
    }

    fn draw_band(&mut self) {
        let Some(b) = self.band.as_ref() else { return };
        if !b.live || b.tab != self.active {
            return;
        }
        let mut r = b.rect(self.tab().scroll);
        r.y -= self.tab().scroll;
        let p = self.p;
        self.cv.tint(r, p.accent, 64);
        self.cv
            .dashed(r, p.accent, (3.0 * self.m.s).max(2.0) as i32);
    }

    /// How fast to scroll for the pointer at `y`: near the top or bottom of
    /// the folder (under the list's header), or past them.
    fn edge_speed_at(&self, y: i32) -> i32 {
        let c = self.content;
        let top = c.y
            + if self.view == View::List {
                self.m.row_h
            } else {
                0
            };
        let zone = (self.m.row_h * 3 / 2).max(12);
        let max = (self.content_height() - c.h).max(0);
        let s = self.tab().scroll;
        let v = crate::band::edge_speed(y, top, c.y + c.h, zone);
        // Nothing to scroll that way: no timer either.
        if (v < 0 && s <= 0) || (v > 0 && s >= max) {
            0
        } else {
            v
        }
    }

    /// One step of scrolling at the edge, while a band or drag is held there.
    fn edge_step(&mut self) {
        if self.edge == 0 {
            self.edge_next = None;
            return;
        }
        let now = Instant::now();
        if self.edge_next.is_some_and(|t| now < t) {
            return;
        }
        self.edge_next = Some(now + EDGE_STEP);
        let before = self.tab().scroll;
        self.tab_mut().scroll += self.edge;
        self.clamp_scroll();
        if self.tab().scroll == before {
            self.edge = 0;
            self.edge_next = None;
            return;
        }
        let (x, y) = self.pointer;
        if self.band.is_some() {
            self.band_update();
        } else if self.drag.is_some() || self.drag_in.is_some() {
            let files = self
                .drag
                .as_ref()
                .map(|d| d.files.clone())
                .unwrap_or_default();
            let at = self.drop_at(x, y, &files);
            self.set_drop_target(at);
        }
        self.edge = self.edge_speed_at(y);
        self.dirty = true;
    }

    /// Where a drop would land now; a folder held there long enough opens.
    fn set_drop_target(&mut self, at: Option<DropAt>) {
        if at != self.drop_target {
            self.spring = match &at {
                Some(DropAt::Here) | None => None,
                Some(a) => Some((a.clone(), Instant::now() + SPRING)),
            };
            self.drop_target = at;
            self.dirty = true;
        }
    }

    /// The folder under a drag has been held long enough: open it, so the
    /// drop can go further down.
    fn spring_open(&mut self) {
        let Some((at, when)) = self.spring.clone() else {
            return;
        };
        if Instant::now() < when || self.drop_target.as_ref() != Some(&at) {
            return;
        }
        self.spring = None;
        match at {
            DropAt::Item(i) => {
                if let Some(e) = self.tab().entry(i).filter(|e| e.is_dir).cloned() {
                    self.go(e.path);
                }
            }
            DropAt::Place(i) => self.open_place(i),
            DropAt::Tab(i) => self.switch(i),
            DropAt::Here => {}
        }
        self.drop_target = Some(DropAt::Here);
        self.dirty = true;
    }

    /// The label beside the pointer: how many files and what a drop does.
    fn drag_label(&mut self, rx: i16, ry: i16, state: KeyButMask) {
        let Some(d) = self.drag.as_ref() else { return };
        let n = d.files.len();
        let what = if n == 1 {
            d.files[0]
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            format!("{n} items")
        };
        let own = d.target.is_some_and(|(w, _)| w == self.x.win);
        let verb = if own {
            match self.drop_target.as_ref().and_then(|a| self.drop_dir(a)) {
                Some(dest) => {
                    use std::os::unix::fs::MetadataExt;
                    let ctrl = state.contains(KeyButMask::CONTROL);
                    let shift = state.contains(KeyButMask::SHIFT);
                    let same_fs = std::fs::metadata(&dest).ok().map(|m| m.dev())
                        == std::fs::symlink_metadata(&d.files[0]).ok().map(|m| m.dev());
                    let name = dest
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "/".into());
                    if d.files[0].parent() == Some(dest.as_path()) {
                        String::new()
                    } else if shift || (same_fs && !ctrl) {
                        format!("move to {name}")
                    } else {
                        format!("copy to {name}")
                    }
                }
                None => String::new(),
            }
        } else if d.accepted {
            if state.contains(KeyButMask::SHIFT) {
                "move".into()
            } else {
                "copy".into()
            }
        } else {
            String::new()
        };
        let text = if verb.is_empty() {
            what
        } else {
            format!("{what} · {verb}")
        };
        if self.label_win.is_none() {
            self.label_win = self.x.label_window();
        }
        let Some(w) = self.label_win else { return };
        let (lx, ly) = (rx as i32 + 18, ry as i32 + 20);
        if text == self.label_text {
            self.x.show_label(w, lx, ly, None);
            return;
        }
        let (small, pad) = (self.m.small, self.m.pad);
        let text = self.text.ellipsize(&text, small, (360.0 * self.m.s) as i32);
        let u = (2.0 * self.m.s).max(2.0) as i32;
        let tw = self.text.width(&text, small);
        let (cw, ch) = (tw + 2 * pad + 3 * u, small.height + pad);
        let mut cv = crate::canvas::Canvas::new(cw, ch);
        let p = self.p;
        cv.fill(Rect::new(0, 0, cw, ch), p.surface);
        cv.frame(Rect::new(0, 0, cw, ch), p.accent);
        // A pixel block of the accent on the left, like a cursor.
        cv.fill(Rect::new(u, u, u, ch - 2 * u), p.accent);
        self.text.draw(
            &mut cv,
            pad + 2 * u,
            (ch - small.height) / 2,
            &text,
            small,
            if verb.is_empty() {
                p.muted
            } else {
                p.foreground
            },
        );
        self.label_text = text;
        self.x.show_label(w, lx, ly, Some(&cv));
    }

    fn hide_drag_label(&mut self) {
        if let Some(w) = self.label_win {
            self.x.hide_label(w);
        }
        self.label_text.clear();
    }

    /// Esc during a drag out of this window: nothing is dropped.
    fn drag_cancel(&mut self) {
        let Some(d) = self.drag.take() else { return };
        self.x.ungrab();
        if let Some((w, _)) = d.target.filter(|t| t.0 != self.x.win) {
            self.x
                .send_client(w, self.x.atoms.xdnd_leave, [self.x.win, 0, 0, 0, 0]);
        }
        self.press = None;
        self.drop_target = None;
        self.spring = None;
        self.edge = 0;
        self.edge_next = None;
        self.hide_drag_label();
        self.flash("Drag cancelled".into());
        self.dirty = true;
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

    #[test]
    fn a_password_is_shown_as_dots_with_its_cursor() {
        assert_eq!(mask("åb"), "••");
        assert_eq!(unmask("åbc", 0), 0);
        assert_eq!(unmask("åbc", 3), 2);
        assert_eq!(unmask("åbc", 6), 3);
        assert_eq!(unmask("åbc", 99), 4);
        let ed = crate::edit::LineEdit::selecting("åbc", 2, 4);
        let m = masked(&ed);
        assert_eq!((m.anchor, m.cursor), (3, 9));
    }

    #[test]
    fn tab_completes_a_folder_name() {
        let d = std::env::temp_dir().join(format!("nirufm-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for sub in ["Music", "Mail one", "Mail two", ".hidden"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        std::fs::write(d.join("Mufile"), "").unwrap();
        let base = format!("{}/", d.display());
        assert_eq!(
            complete_folder(&format!("{base}Mu"), false),
            Some(format!("{base}Music/"))
        );
        assert_eq!(
            complete_folder(&format!("{base}Ma"), false),
            Some(format!("{base}Mail "))
        );
        assert_eq!(complete_folder(&format!("{base}x"), false), None);
        assert_eq!(
            complete_folder(&format!("{base}.h"), false),
            Some(format!("{base}.hidden/"))
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
