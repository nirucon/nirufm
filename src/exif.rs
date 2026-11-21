//! A small reader for the EXIF facts the gallery shows: camera, date,
//! exposure, aperture, ISO and focal length, from a JPEG's APP1 segment.
use std::io::Read;
use std::path::Path;

#[derive(Default, Debug, Clone, PartialEq)]
pub struct Exif {
    pub camera: Option<String>,
    pub date: Option<String>,
    pub exposure: Option<String>,
    pub aperture: Option<String>,
    pub iso: Option<u32>,
    pub focal: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Exif {
    pub fn line(&self) -> String {
        let mut v = Vec::new();
        if let Some(c) = &self.camera {
            v.push(c.clone());
        }
        if let Some(d) = &self.date {
            v.push(d.clone());
        }
        let mut shot = Vec::new();
        if let Some(a) = &self.aperture {
            shot.push(a.clone());
        }
        if let Some(e) = &self.exposure {
            shot.push(e.clone());
        }
        if let Some(i) = self.iso {
            shot.push(format!("ISO {i}"));
        }
        if let Some(f) = &self.focal {
            shot.push(f.clone());
        }
        if !shot.is_empty() {
            v.push(shot.join(" "));
        }
        v.join(" · ")
    }
}

pub fn read(path: &Path) -> Option<Exif> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = vec![0u8; 256 * 1024];
    let n = f.read(&mut head).ok()?;
    head.truncate(n);
    parse_jpeg(&head)
}

fn parse_jpeg(b: &[u8]) -> Option<Exif> {
    if b.get(0..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xff {
            return None;
        }
        let marker = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if marker == 0xe1 && b.get(i + 4..i + 10)? == b"Exif\0\0" {
            return parse_tiff(b.get(i + 10..i + 2 + len)?);
        }
        if marker == 0xda {
            return None;
        }
        i += 2 + len;
    }
    None
}

struct Tiff<'a> {
    b: &'a [u8],
    le: bool,
}

impl Tiff<'_> {
    fn u16(&self, o: usize) -> Option<u16> {
        let s = self.b.get(o..o + 2)?;
        Some(if self.le {
            u16::from_le_bytes([s[0], s[1]])
        } else {
            u16::from_be_bytes([s[0], s[1]])
        })
    }
    fn u32(&self, o: usize) -> Option<u32> {
        let s = self.b.get(o..o + 4)?;
        let a = [s[0], s[1], s[2], s[3]];
        Some(if self.le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    }
    /// Entries of the IFD at `o`: (tag, type, count, value offset field).
    fn ifd(&self, o: usize) -> Vec<(u16, u16, u32, usize)> {
        let Some(n) = self.u16(o) else {
            return Vec::new();
        };
        (0..n as usize)
            .filter_map(|k| {
                let e = o + 2 + k * 12;
                Some((self.u16(e)?, self.u16(e + 2)?, self.u32(e + 4)?, e + 8))
            })
            .collect()
    }
    fn ascii(&self, ty: u16, count: u32, field: usize) -> Option<String> {
        if ty != 2 {
            return None;
        }
        let at = if count <= 4 {
            field
        } else {
            self.u32(field)? as usize
        };
        let s = self.b.get(at..at + count as usize)?;
        let s = String::from_utf8_lossy(s)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        (!s.is_empty()).then_some(s)
    }
    fn rational(&self, field: usize) -> Option<(u32, u32)> {
        let at = self.u32(field)? as usize;
        Some((self.u32(at)?, self.u32(at + 4)?))
    }
    fn number(&self, ty: u16, field: usize) -> Option<u32> {
        match ty {
            3 => self.u16(field).map(u32::from),
            4 => self.u32(field),
            _ => None,
        }
    }
}

fn parse_tiff(b: &[u8]) -> Option<Exif> {
    let le = match b.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let t = Tiff { b, le };
    let ifd0 = t.u32(4)? as usize;
    let mut x = Exif::default();
    let (mut make, mut model) = (None, None);
    let mut sub = None;
    for (tag, ty, count, field) in t.ifd(ifd0) {
        match tag {
            0x010f => make = t.ascii(ty, count, field),
            0x0110 => model = t.ascii(ty, count, field),
            0x0132 => x.date = t.ascii(ty, count, field),
            0x8769 => sub = t.u32(field).map(|v| v as usize),
            _ => {}
        }
    }
    if let Some(o) = sub {
        for (tag, ty, count, field) in t.ifd(o) {
            match tag {
                0x9003 => x.date = t.ascii(ty, count, field).or(x.date.take()),
                0x829a => {
                    if let Some((n, d)) = t.rational(field).filter(|(_, d)| *d > 0) {
                        x.exposure = Some(if n >= d {
                            format!("{:.1}s", n as f64 / d as f64)
                        } else {
                            format!("1/{}s", (d as f64 / n.max(1) as f64).round())
                        });
                    }
                }
                0x829d => {
                    if let Some((n, d)) = t.rational(field).filter(|(_, d)| *d > 0) {
                        x.aperture = Some(format!("f/{:.1}", n as f64 / d as f64));
                    }
                }
                0x8827 => x.iso = t.number(ty, field),
                0x920a => {
                    if let Some((n, d)) = t.rational(field).filter(|(_, d)| *d > 0) {
                        x.focal = Some(format!("{:.0}mm", n as f64 / d as f64));
                    }
                }
                0xa002 => x.width = t.number(ty, field),
                0xa003 => x.height = t.number(ty, field),
                _ => {}
            }
        }
    }
    // "2026:08:24 07:28:11" reads better as "2026-08-24 07:28".
    if let Some(d) = &x.date {
        if d.len() >= 16 {
            x.date = Some(format!(
                "{}-{}-{} {}",
                &d[0..4],
                &d[5..7],
                &d[8..10],
                &d[11..16]
            ));
        }
    }
    x.camera = match (make, model) {
        (Some(mk), Some(md)) if md.starts_with(&mk) => Some(md),
        (Some(mk), Some(md)) => Some(format!("{mk} {md}")),
        (a, b) => b.or(a),
    };
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal big-endian EXIF block with model, date and ISO.
    #[test]
    fn camera_date_and_iso_are_read() {
        let mut t: Vec<u8> = b"MM\0\x2a\0\0\0\x08".to_vec();
        // IFD0: 2 entries (Model, ExifIFD), next 0
        let model = b"X100V\0";
        let ifd0_len = 2 + 2 * 12 + 4;
        let model_at = 8 + ifd0_len;
        let exif_at = model_at + model.len();
        t.extend_from_slice(&2u16.to_be_bytes());
        t.extend_from_slice(&[0x01, 0x10, 0, 2]);
        t.extend_from_slice(&(model.len() as u32).to_be_bytes());
        t.extend_from_slice(&(model_at as u32).to_be_bytes());
        t.extend_from_slice(&[0x87, 0x69, 0, 4, 0, 0, 0, 1]);
        t.extend_from_slice(&(exif_at as u32).to_be_bytes());
        t.extend_from_slice(&[0, 0, 0, 0]);
        t.extend_from_slice(model);
        // Exif IFD: DateTimeOriginal + ISO
        let date = b"2026:08:24 07:28:11\0";
        let date_at = exif_at + 2 + 2 * 12 + 4;
        t.extend_from_slice(&2u16.to_be_bytes());
        t.extend_from_slice(&[0x90, 0x03, 0, 2]);
        t.extend_from_slice(&(date.len() as u32).to_be_bytes());
        t.extend_from_slice(&(date_at as u32).to_be_bytes());
        t.extend_from_slice(&[0x88, 0x27, 0, 3, 0, 0, 0, 1, 0x01, 0x90, 0, 0]);
        t.extend_from_slice(&[0, 0, 0, 0]);
        t.extend_from_slice(date);
        let x = parse_tiff(&t).unwrap();
        assert_eq!(x.camera.as_deref(), Some("X100V"));
        assert_eq!(x.date.as_deref(), Some("2026-08-24 07:28"));
        assert_eq!(x.iso, Some(400));
        assert_eq!(x.line(), "X100V · 2026-08-24 07:28 · ISO 400");
    }
}
