//! Comparing the folders of the two panes by name: what is only on one
//! side, and files on both sides that differ in size or time.
use std::collections::HashMap;

/// How an item stands against the other pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Not in the other folder.
    Only,
    /// In both, but a different size or time; `newer` when this side's
    /// copy is the more recent one.
    Differs { newer: bool },
}

/// What is compared of an item.
pub struct Item<'a> {
    pub name: &'a str,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

/// File systems such as FAT keep times in steps of two seconds, so copies
/// within that are the same.
const SLACK: i64 = 2;

/// The marks of each side, by name. Folders on both sides count as the
/// same (their contents are not walked); a folder against a file of the
/// same name differs.
pub fn compare(left: &[Item], right: &[Item]) -> (HashMap<String, Mark>, HashMap<String, Mark>) {
    let index = |v: &[Item]| -> HashMap<String, usize> {
        v.iter()
            .enumerate()
            .map(|(k, e)| (e.name.to_string(), k))
            .collect()
    };
    let (li, ri) = (index(left), index(right));
    let side = |mine: &[Item], theirs: &[Item], other: &HashMap<String, usize>| {
        let mut out = HashMap::new();
        for e in mine {
            let Some(&k) = other.get(e.name) else {
                out.insert(e.name.to_string(), Mark::Only);
                continue;
            };
            let o = &theirs[k];
            let mark = if e.is_dir != o.is_dir {
                Some(Mark::Differs {
                    newer: e.mtime > o.mtime,
                })
            } else if e.is_dir {
                None
            } else if e.size != o.size || (e.mtime - o.mtime).abs() > SLACK {
                Some(Mark::Differs {
                    newer: e.mtime > o.mtime + SLACK
                        || ((e.mtime - o.mtime).abs() <= SLACK && e.size != o.size),
                })
            } else {
                None
            };
            if let Some(m) = mark {
                out.insert(e.name.to_string(), m);
            }
        }
        out
    };
    (side(left, right, &ri), side(right, left, &li))
}

/// Whether an item with this mark is selected to be copied across: what
/// the other side lacks, and the newer copy of what differs.
pub fn wanted(m: Mark) -> bool {
    matches!(m, Mark::Only | Mark::Differs { newer: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(name: &str, is_dir: bool, size: u64, mtime: i64) -> Item<'_> {
        Item {
            name,
            is_dir,
            size,
            mtime,
        }
    }

    #[test]
    fn only_on_one_side_and_differences() {
        let left = [
            it("same.txt", false, 5, 100),
            it("only-left.txt", false, 1, 100),
            it("newer-left.txt", false, 5, 500),
            it("bigger.txt", false, 9, 100),
            it("dir", true, 0, 1),
            it("fat.txt", false, 5, 101),
            it("kind", true, 0, 100),
        ];
        let right = [
            it("same.txt", false, 5, 100),
            it("newer-left.txt", false, 5, 100),
            it("bigger.txt", false, 5, 100),
            it("dir", true, 0, 999),
            it("only-right", true, 0, 1),
            it("fat.txt", false, 5, 100),
            it("kind", false, 3, 50),
        ];
        let (l, r) = compare(&left, &right);
        assert_eq!(l.get("same.txt"), None);
        assert_eq!(l.get("dir"), None);
        assert_eq!(l.get("fat.txt"), None);
        assert_eq!(l["only-left.txt"], Mark::Only);
        assert_eq!(l["newer-left.txt"], Mark::Differs { newer: true });
        assert_eq!(r["newer-left.txt"], Mark::Differs { newer: false });
        // Same time, other size: both sides are offered.
        assert_eq!(l["bigger.txt"], Mark::Differs { newer: true });
        assert_eq!(r["bigger.txt"], Mark::Differs { newer: true });
        assert_eq!(r["only-right"], Mark::Only);
        assert_eq!(l["kind"], Mark::Differs { newer: true });
        assert_eq!(l.len(), 4);
        assert_eq!(r.len(), 4);
        assert_eq!(r["kind"], Mark::Differs { newer: false });
        assert!(wanted(Mark::Only));
        assert!(!wanted(r["newer-left.txt"]));
    }
}
