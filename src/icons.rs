//! Icons from the freedesktop icon theme (Papirus in NIRUX), rendered once
//! per (name, size) and kept. SVG goes through resvg; PNG is decoded and
//! scaled. A light NIRU theme switches to the light icon set.
use crate::canvas::Image;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct Icons {
    themes: Vec<String>,
    dirs: Vec<PathBuf>,
    files: HashMap<String, Option<PathBuf>>,
    cache: HashMap<(String, i32), Option<std::rc::Rc<Image>>>,
}

const CONTEXTS: [&str; 7] = [
    "places",
    "mimetypes",
    "devices",
    "apps",
    "status",
    "actions",
    "categories",
];

impl Icons {
    pub fn new(light: bool) -> Self {
        let mut i = Self {
            themes: Vec::new(),
            dirs: icon_dirs(),
            files: HashMap::new(),
            cache: HashMap::new(),
        };
        i.set_light(light);
        i
    }

    pub fn set_light(&mut self, light: bool) {
        let user = gtk_icon_theme();
        let mut themes = Vec::new();
        let papirus = if light {
            "Papirus-Light"
        } else {
            "Papirus-Dark"
        };
        // A Papirus variant that does not fit the background is swapped for
        // the right one; any other theme the user picked is kept first.
        match user.as_deref() {
            Some(t) if t.starts_with("Papirus") => themes.push(papirus.to_string()),
            Some(t) => themes.push(t.to_string()),
            None => themes.push(papirus.to_string()),
        }
        for t in [papirus, "Papirus", "Adwaita", "hicolor"] {
            if !themes.iter().any(|x| x == t) {
                themes.push(t.to_string());
            }
        }
        if themes != self.themes {
            self.themes = themes;
            self.files.clear();
            self.cache.clear();
        }
    }

    fn find(&mut self, name: &str, size: i32) -> Option<PathBuf> {
        let key = format!("{name}@{}", size_bucket(size));
        if let Some(hit) = self.files.get(&key) {
            return hit.clone();
        }
        let mut found = None;
        let sizes = size_dirs(size);
        'outer: for theme in &self.themes {
            for base in &self.dirs {
                let root = base.join(theme);
                if !root.is_dir() {
                    continue;
                }
                for sz in &sizes {
                    for ctx in CONTEXTS {
                        for (dir, ext) in [
                            (format!("{sz}/{ctx}"), "svg"),
                            (format!("{ctx}/{sz}"), "svg"),
                            (format!("{sz}/{ctx}"), "png"),
                            (format!("{ctx}/{sz}"), "png"),
                        ] {
                            let p = root.join(&dir).join(format!("{name}.{ext}"));
                            if p.is_file() {
                                found = Some(p);
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
        if found.is_none() {
            let p = PathBuf::from("/usr/share/pixmaps").join(format!("{name}.png"));
            if p.is_file() {
                found = Some(p);
            }
        }
        self.files.insert(key, found.clone());
        found
    }

    /// The first of `names` the theme has, rendered at `size` pixels.
    pub fn get(&mut self, names: &[String], size: i32) -> Option<std::rc::Rc<Image>> {
        for n in names {
            let key = (n.clone(), size);
            if let Some(hit) = self.cache.get(&key) {
                if hit.is_some() {
                    return hit.clone();
                }
                continue;
            }
            let img = self
                .find(n, size)
                .and_then(|p| render_file(&p, size))
                .map(std::rc::Rc::new);
            self.cache.insert(key, img.clone());
            if img.is_some() {
                return img;
            }
        }
        None
    }

    pub fn named(&mut self, name: &str, size: i32) -> Option<std::rc::Rc<Image>> {
        self.get(&[name.to_string()], size)
    }
}

fn size_bucket(size: i32) -> i32 {
    [16, 22, 24, 32, 48, 64, 96, 128]
        .into_iter()
        .find(|s| *s >= size)
        .unwrap_or(128)
}

fn size_dirs(size: i32) -> Vec<String> {
    let b = size_bucket(size);
    let mut v = vec![format!("{b}x{b}"), format!("{b}x{b}@2x")];
    for s in [48, 64, 32, 128, 24, 22, 16] {
        if s != b {
            v.push(format!("{s}x{s}"));
        }
    }
    v.push("scalable".into());
    v
}

fn icon_dirs() -> Vec<PathBuf> {
    let mut v = vec![crate::theme::home().join(".icons")];
    for d in crate::mime::data_dirs() {
        v.push(d.join("icons"));
    }
    v
}

fn gtk_icon_theme() -> Option<String> {
    let path = crate::theme::config_home().join("gtk-3.0/settings.ini");
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("gtk-icon-theme-name"))
        .map(|rest| {
            rest.trim_start_matches([' ', '='])
                .trim()
                .trim_matches('"')
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

pub fn render_file(path: &std::path::Path, size: i32) -> Option<Image> {
    let data = std::fs::read(path).ok()?;
    if path.extension().is_some_and(|e| e == "svg") {
        render_svg(&data, size)
    } else {
        let img = decode_png(&data)?;
        Some(scale_to_fit(&img, size, size))
    }
}

pub fn render_svg(data: &[u8], size: i32) -> Option<Image> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let ts = tree.size();
    let scale = (size as f32 / ts.width()).min(size as f32 / ts.height());
    let mut pm = tiny_skia::Pixmap::new(size as u32, size as u32)?;
    let dx = (size as f32 - ts.width() * scale) / 2.0;
    let dy = (size as f32 - ts.height() * scale) / 2.0;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
        &mut pm.as_mut(),
    );
    Some(Image {
        w: size,
        h: size,
        data: pm.take(),
    })
}

/// PNG to premultiplied RGBA.
pub fn decode_png(data: &[u8]) -> Option<Image> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as i32, info.height as i32);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    let channels = info.color_type.samples();
    for px in buf[..info.buffer_size()].chunks(channels) {
        let (r, g, b, a) = match channels {
            4 => (px[0], px[1], px[2], px[3]),
            2 => (px[0], px[0], px[0], px[1]),
            3 => (px[0], px[1], px[2], 255),
            _ => (px[0], px[0], px[0], 255),
        };
        let pm = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
        out.extend_from_slice(&[pm(r), pm(g), pm(b), a]);
    }
    Some(Image { w, h, data: out })
}

/// Box-filter scale of a premultiplied image to fit (w, h), keeping aspect.
pub fn scale_to_fit(img: &Image, w: i32, h: i32) -> Image {
    if img.w <= w && img.h <= h {
        return Image {
            w: img.w,
            h: img.h,
            data: img.data.clone(),
        };
    }
    let s = (w as f32 / img.w as f32).min(h as f32 / img.h as f32);
    let nw = ((img.w as f32 * s).round() as i32).max(1);
    let nh = ((img.h as f32 * s).round() as i32).max(1);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        let sy0 = y * img.h / nh;
        let sy1 = ((y + 1) * img.h / nh).max(sy0 + 1);
        for x in 0..nw {
            let sx0 = x * img.w / nw;
            let sx1 = ((x + 1) * img.w / nw).max(sx0 + 1);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in sy0..sy1 {
                let row = (sy * img.w) as usize * 4;
                for sx in sx0..sx1 {
                    let i = row + sx as usize * 4;
                    for (a, v) in acc.iter_mut().zip(&img.data[i..i + 4]) {
                        *a += *v as u32;
                    }
                    n += 1;
                }
            }
            let o = ((y * nw + x) * 4) as usize;
            for (dst, a) in out[o..o + 4].iter_mut().zip(acc) {
                *dst = (a / n.max(1)) as u8;
            }
        }
    }
    Image {
        w: nw,
        h: nh,
        data: out,
    }
}

/// Turn a premultiplied image by quarter turns clockwise.
pub fn rotate(img: &Image, quarters: u8) -> Image {
    let q = quarters % 4;
    if q == 0 {
        return Image {
            w: img.w,
            h: img.h,
            data: img.data.clone(),
        };
    }
    let (w, h) = (img.w as usize, img.h as usize);
    let (nw, nh) = if q % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match q {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let s = (y * w + x) * 4;
            let d = (ny * nw + nx) * 4;
            out[d..d + 4].copy_from_slice(&img.data[s..s + 4]);
        }
    }
    Image {
        w: nw as i32,
        h: nh as i32,
        data: out,
    }
}
