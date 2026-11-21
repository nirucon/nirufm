//! Thumbnails the freedesktop way, shared with every other file manager:
//! `~/.cache/thumbnails/{normal,large}/MD5(uri).png`, made by the
//! thumbnailers the system declares in `/usr/share/thumbnailers` (gdk-pixbuf
//! for pictures, ffmpegthumbnailer for video, evince/poppler for PDF...).
//! Work happens on two background threads; the window only draws results.
use crate::canvas::Image;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

pub struct Request {
    pub path: PathBuf,
    pub mime: String,
    pub mtime: i64,
    pub size: u32,
}

pub struct Done {
    pub path: PathBuf,
    pub image: Option<Image>,
}

pub struct Thumbs {
    queue: Arc<Mutex<Vec<Request>>>,
    signal: Sender<()>,
    pub rx: Receiver<Done>,
}

#[derive(Clone)]
struct Thumbnailer {
    exec: String,
}

fn load_thumbnailers() -> HashMap<String, Thumbnailer> {
    let mut map = HashMap::new();
    for d in crate::mime::data_dirs().iter().rev() {
        let Ok(rd) = std::fs::read_dir(d.join("thumbnailers")) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let get = |k: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(k))
                    .map(|v| v.trim().to_string())
            };
            let (Some(exec), Some(types)) = (get("Exec="), get("MimeType=")) else {
                continue;
            };
            for t in types.split(';').filter(|t| !t.is_empty()) {
                map.insert(t.to_string(), Thumbnailer { exec: exec.clone() });
            }
        }
    }
    map
}

/// The Exec line of the system thumbnailer for a type.
pub fn thumbnailer_for(mime: &str) -> Option<String> {
    load_thumbnailers().get(mime).map(|t| t.exec.clone())
}

pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::theme::home().join(".cache"))
        .join("thumbnails")
}

/// Whether a thumbnail can be made for this type at all.
pub fn wanted(mime: &str, have: &HashMap<String, ()>) -> bool {
    mime == "image/png" || have.contains_key(mime)
}

impl Thumbs {
    pub fn start(wake: i32) -> (Self, HashMap<String, ()>) {
        let thumbnailers = Arc::new(load_thumbnailers());
        let types: HashMap<String, ()> = thumbnailers.keys().map(|k| (k.clone(), ())).collect();
        let queue: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let (signal, sig_rx) = std::sync::mpsc::channel::<()>();
        let sig_rx = Arc::new(Mutex::new(sig_rx));
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..2 {
            let (queue, sig_rx, tx, t) = (
                queue.clone(),
                sig_rx.clone(),
                tx.clone(),
                thumbnailers.clone(),
            );
            std::thread::spawn(move || loop {
                // Newest requests first: what is on screen now matters most.
                let job = queue.lock().ok().and_then(|mut q| q.pop());
                let Some(job) = job else {
                    if sig_rx.lock().map(|r| r.recv().is_err()).unwrap_or(true) {
                        return;
                    }
                    continue;
                };
                let image = make(&job, &t);
                let _ = tx.send(Done {
                    path: job.path,
                    image,
                });
                crate::ops::poke(wake);
            });
        }
        (Self { queue, signal, rx }, types)
    }

    pub fn request(&self, r: Request) {
        if let Ok(mut q) = self.queue.lock() {
            q.push(r);
        }
        let _ = self.signal.send(());
    }

    /// Forget queued work for a folder that is no longer shown.
    pub fn clear(&self) {
        if let Ok(mut q) = self.queue.lock() {
            q.clear();
        }
    }
}

fn make(r: &Request, t: &HashMap<String, Thumbnailer>) -> Option<Image> {
    let uri = crate::x::file_uri(&r.path);
    let (flavour, px) = if r.size > 128 {
        ("large", 256)
    } else {
        ("normal", 128)
    };
    let name = format!("{}.png", md5_hex(uri.as_bytes()));
    let cached = cache_dir().join(flavour).join(&name);
    if let Some(img) = read_fresh(&cached, r.mtime) {
        return Some(img);
    }
    if cache_dir().join("fail/nirufm").join(&name).exists() {
        return None;
    }
    let _ = std::fs::create_dir_all(cached.parent()?);
    let tmp = cached.with_extension(format!("nirufm-{}.png", std::process::id()));
    let ok = if let Some(th) = t.get(&r.mime) {
        run_thumbnailer(&th.exec, &r.path, &uri, &tmp, px)
    } else if r.mime == "image/png" {
        own_png(&r.path, &tmp, px as i32, &uri, r.mtime)
    } else {
        false
    };
    if ok {
        stamp(&tmp, &uri, r.mtime);
        let _ = std::fs::rename(&tmp, &cached);
        read_fresh(&cached, r.mtime).or_else(|| load_png(&cached))
    } else {
        let _ = std::fs::remove_file(&tmp);
        let fail = cache_dir().join("fail/nirufm");
        let _ = std::fs::create_dir_all(&fail);
        let _ = std::fs::write(fail.join(&name), b"");
        None
    }
}

fn load_png(p: &Path) -> Option<Image> {
    crate::icons::decode_png(&std::fs::read(p).ok()?)
}

/// A cached thumbnail, if it is newer than the file (Thumb::MTime matches,
/// or for thumbnails without it, a cache file at least as new).
fn read_fresh(p: &Path, mtime: i64) -> Option<Image> {
    let data = std::fs::read(p).ok()?;
    match png_text(&data, "Thumb::MTime") {
        Some(v) if v.parse::<i64>().ok() != Some(mtime) => return None,
        None => {
            let cm = std::fs::metadata(p).ok()?;
            use std::os::unix::fs::MetadataExt;
            if cm.mtime() < mtime {
                return None;
            }
        }
        _ => {}
    }
    crate::icons::decode_png(&data)
}

/// A tEXt chunk value from PNG bytes.
fn png_text(data: &[u8], key: &str) -> Option<String> {
    let mut i = 8;
    while i + 12 <= data.len() {
        let len = u32::from_be_bytes(data[i..i + 4].try_into().ok()?) as usize;
        let kind = &data[i + 4..i + 8];
        let body = data.get(i + 8..i + 8 + len)?;
        if kind == b"tEXt" {
            if let Some(z) = body.iter().position(|b| *b == 0) {
                if &body[..z] == key.as_bytes() {
                    return Some(String::from_utf8_lossy(&body[z + 1..]).into_owned());
                }
            }
        }
        if kind == b"IDAT" {
            break;
        }
        i += 12 + len;
    }
    None
}

pub fn run_thumbnailer(exec: &str, path: &Path, uri: &str, out: &Path, size: u32) -> bool {
    let mut args: Vec<String> = Vec::new();
    for word in exec.split_whitespace() {
        let w = word
            .replace("%s", &size.to_string())
            .replace("%u", uri)
            .replace("%i", &path.to_string_lossy())
            .replace("%o", &out.to_string_lossy());
        args.push(w);
    }
    let Some((cmd, rest)) = args.split_first() else {
        return false;
    };
    let mut c = std::process::Command::new(cmd);
    c.args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = c.spawn() else {
        return false;
    };
    // A broken file must not hang a worker for long.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => return st.success() && out.exists(),
            Ok(None) if start.elapsed().as_secs() > 15 => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return false,
        }
    }
}

fn own_png(src: &Path, out: &Path, px: i32, _uri: &str, _mtime: i64) -> bool {
    let Some(img) = std::fs::read(src)
        .ok()
        .and_then(|d| crate::icons::decode_png(&d))
    else {
        return false;
    };
    let small = crate::icons::scale_to_fit(&img, px, px);
    write_png(out, &small).is_ok()
}

pub fn write_png(out: &Path, img: &Image) -> std::io::Result<()> {
    let f = std::fs::File::create(out)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.w as u32, img.h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(std::io::Error::other)?;
    // Back from premultiplied alpha.
    let mut straight = img.data.clone();
    for px in straight.chunks_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    w.write_image_data(&straight)
        .map_err(std::io::Error::other)?;
    Ok(())
}

/// Add Thumb::URI and Thumb::MTime when the thumbnailer did not.
fn stamp(p: &Path, uri: &str, mtime: i64) {
    let Ok(data) = std::fs::read(p) else { return };
    if data.len() < 33 || png_text(&data, "Thumb::MTime").is_some() {
        return;
    }
    let mut out = data[..33].to_vec(); // signature + IHDR
    for (k, v) in [
        ("Thumb::URI", uri.to_string()),
        ("Thumb::MTime", mtime.to_string()),
    ] {
        let mut body = k.as_bytes().to_vec();
        body.push(0);
        body.extend_from_slice(v.as_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut chunk = b"tEXt".to_vec();
        chunk.extend_from_slice(&body);
        out.extend_from_slice(&chunk);
        out.extend_from_slice(&crc32(&chunk).to_be_bytes());
    }
    out.extend_from_slice(&data[33..]);
    let _ = std::fs::write(p, out);
}

fn crc32(b: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &x in b {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// MD5 (RFC 1321), as the thumbnail spec names files by it.
pub fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_the_rfc_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn written_thumbnails_carry_uri_and_mtime() {
        let d = std::env::temp_dir().join(format!("nirufm-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("t.png");
        write_png(
            &p,
            &Image {
                w: 2,
                h: 2,
                data: vec![255; 16],
            },
        )
        .unwrap();
        stamp(&p, "file:///x", 1234);
        let data = std::fs::read(&p).unwrap();
        assert_eq!(png_text(&data, "Thumb::MTime").as_deref(), Some("1234"));
        assert_eq!(png_text(&data, "Thumb::URI").as_deref(), Some("file:///x"));
        assert!(crate::icons::decode_png(&data).is_some());
        std::fs::remove_dir_all(d).unwrap();
    }
}
