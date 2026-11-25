//! NIRUFM: a fast, light file manager for the NIRU suite.
mod app;
mod apps;
mod archive;
mod band;
mod canvas;
mod devices;
mod dir;
mod edit;
mod exif;
mod icons;
mod logo;
mod mime;
mod nextcloud;
mod ops;
mod places;
mod remote;
mod text;
mod theme;
mod thumbs;
mod x;

use std::path::PathBuf;

const HELP: &str = "NIRUFM: the NIRU file manager

  nirufm [FOLDER]        open FOLDER (default: the home folder)
  nirufm --version

Keys: Enter open · Backspace up · Alt+Left/Right back/forward
      Ctrl+T new tab · Ctrl+W close tab · Ctrl+Tab next tab
      Ctrl+L type a path (Tab completes) · Ctrl+H hidden files
      Ctrl+1 icons · Ctrl+2 list · Ctrl+A select all · F4 terminal
      Ctrl+Shift+N new folder · F5 reload · type to find
Mouse: double-click open · middle-click folder or place: new tab
       middle-click a tab: close · back/forward buttons";

fn main() {
    let started = std::time::Instant::now();
    let mut start: Option<PathBuf> = None;
    for a in std::env::args_os().skip(1) {
        match a.to_str() {
            Some("--version") | Some("-V") => {
                println!("NIRUFM {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            Some("--help") | Some("-h") => {
                println!("{HELP}");
                return;
            }
            _ => {
                // file:// URIs come from other programs ("show in folder").
                let s = a.to_string_lossy();
                let p = s
                    .strip_prefix("file://")
                    .map(|r| PathBuf::from(percent_decode(r)))
                    .unwrap_or_else(|| PathBuf::from(&a));
                start = Some(p);
            }
        }
    }
    let mut path = start.unwrap_or_else(theme::home);
    if path.is_file() {
        path = path.parent().map(PathBuf::from).unwrap_or(path);
    }
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    match app::App::new(path, started).and_then(|mut a| a.run()) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("nirufm: {e}");
            std::process::exit(1);
        }
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
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
    String::from_utf8_lossy(&out).into_owned()
}
