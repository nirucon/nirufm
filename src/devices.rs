//! USB sticks, SD cards and other removable drives through udisks2
//! (`udisksctl`), the same service desktop environments use, so no root
//! rights are needed. `lsblk` lists what is plugged in.
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq)]
pub struct Drive {
    pub dev: String,
    pub label: String,
    pub size: String,
    pub mount: Option<PathBuf>,
    pub removable: bool,
}

/// `KEY="value" KEY2="value"` pairs of `lsblk -P`.
fn pairs(line: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    let mut rest = line.trim();
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].trim().to_string();
        let after = &rest[eq + 2..];
        let Some(end) = after.find('"') else { break };
        v.push((key, unescape(&after[..end])));
        rest = &after[end + 1..];
    }
    v
}

/// lsblk escapes unsafe bytes as \xNN.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() + 1 && b.get(i + 1) == Some(&b'x') {
            if let Ok(n) = u8::from_str_radix(&s[i + 2..(i + 4).min(s.len())], 16) {
                out.push(n);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse(text: &str) -> Vec<Drive> {
    let mut v = Vec::new();
    for line in text.lines() {
        let kv = pairs(line);
        let get = |k: &str| {
            kv.iter()
                .find(|(a, _)| a == k)
                .map(|(_, b)| b.clone())
                .unwrap_or_default()
        };
        let fstype = get("FSTYPE");
        let kind = get("TYPE");
        let hot = get("RM") == "1" || get("HOTPLUG") == "1";
        if !hot
            || fstype.is_empty()
            || matches!(fstype.as_str(), "swap" | "crypto_LUKS" | "LVM2_member")
        {
            continue;
        }
        if !matches!(kind.as_str(), "part" | "disk") {
            continue;
        }
        let mount = Some(get("MOUNTPOINT"))
            .filter(|m| !m.is_empty())
            .map(PathBuf::from);
        let label = Some(get("LABEL"))
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| {
                let size = get("SIZE");
                format!("{size} drive")
            });
        v.push(Drive {
            dev: get("PATH"),
            label,
            size: get("SIZE"),
            mount,
            removable: true,
        });
    }
    v
}

pub fn removable() -> Vec<Drive> {
    let out = Command::new("lsblk")
        .args([
            "-P",
            "-o",
            "PATH,LABEL,SIZE,MOUNTPOINT,RM,HOTPLUG,TYPE,FSTYPE",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => parse(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

fn udisks(args: &[&str]) -> Result<String, String> {
    let o = Command::new("udisksctl")
        .args(args)
        .arg("--no-user-interaction")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("udisksctl: {e} (install udisks2)"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        let e = String::from_utf8_lossy(&o.stderr);
        Err(e
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed")
            .trim()
            .to_string())
    }
}

/// "Mounted /dev/sdb1 at /media/niru/STICK" -> the mount point.
pub fn mount(dev: &str) -> Result<PathBuf, String> {
    let out = udisks(&["mount", "-b", dev])?;
    out.split(" at ")
        .nth(1)
        .map(|p| PathBuf::from(p.trim().trim_end_matches('.')))
        .ok_or_else(|| format!("unexpected answer: {}", out.trim()))
}

/// Unmount, then power off the whole disk so it can be pulled out.
pub fn eject(dev: &str) -> Result<(), String> {
    udisks(&["unmount", "-b", dev])?;
    let disk = Command::new("lsblk")
        .args(["-no", "PKNAME", dev])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|k| format!("/dev/{k}"))
        .unwrap_or_else(|| dev.to_string());
    // Some readers refuse power-off; the drive is unmounted either way.
    let _ = udisks(&["power-off", "-b", &disk]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsblk_lines_become_removable_drives_only() {
        let text = r#"PATH="/dev/nvme0n1p2" LABEL="" SIZE="900G" MOUNTPOINT="/" RM="0" HOTPLUG="0" TYPE="part" FSTYPE="ext4"
PATH="/dev/sdb" LABEL="" SIZE="29G" MOUNTPOINT="" RM="1" HOTPLUG="1" TYPE="disk" FSTYPE=""
PATH="/dev/sdb1" LABEL="MY\x20STICK" SIZE="29G" MOUNTPOINT="" RM="1" HOTPLUG="1" TYPE="part" FSTYPE="vfat"
PATH="/dev/sdc1" LABEL="" SIZE="1T" MOUNTPOINT="/media/niru/1tb" RM="0" HOTPLUG="1" TYPE="part" FSTYPE="exfat""#;
        let d = parse(text);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].label, "MY STICK");
        assert_eq!(d[0].mount, None);
        assert_eq!(d[1].mount, Some(PathBuf::from("/media/niru/1tb")));
        assert_eq!(d[1].label, "1T drive");
    }
}
