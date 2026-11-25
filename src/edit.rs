//! A one-line text field: a cursor, a selection, word jumps and an undo
//! history of its own, as in the text fields of other programs. Positions
//! are byte offsets that always sit on a character boundary.
//!
//! Also the rules for a file name, shared by renaming one item and many.

/// What a key did to the field, for the owner to act on.
#[derive(Debug, PartialEq, Eq)]
pub enum Key {
    /// The text, cursor or selection changed (or nothing needed doing).
    Edited,
    Commit,
    Cancel,
    Copy,
    Cut,
    Paste,
    /// Tab and Shift+Tab: the owner may move on to another field or item.
    Next(bool),
    /// Not a key for a text field.
    Ignored,
}

#[derive(Clone, Debug, Default)]
pub struct LineEdit {
    pub text: String,
    /// Where the cursor is; the selection runs from `anchor` to here.
    pub cursor: usize,
    pub anchor: usize,
    /// Earlier states for Ctrl+Z: (text, cursor, anchor).
    history: Vec<(String, usize, usize)>,
    /// The last change was typing, so the next character joins its undo step.
    typing: bool,
}

impl LineEdit {
    pub fn new(text: &str) -> Self {
        let n = text.len();
        Self {
            text: text.to_string(),
            cursor: n,
            anchor: n,
            ..Self::default()
        }
    }

    /// A field with `start..end` selected and the cursor at the end of it.
    pub fn selecting(text: &str, start: usize, end: usize) -> Self {
        let mut e = Self::new(text);
        e.anchor = floor(text, start.min(text.len()));
        e.cursor = floor(text, end.min(text.len()));
        e
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.cursor), self.anchor.max(self.cursor))
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.cursor
    }

    pub fn selected(&self) -> &str {
        let (a, b) = self.selection();
        &self.text[a..b]
    }

    fn remember(&mut self) {
        self.history
            .push((self.text.clone(), self.cursor, self.anchor));
        // Names are short; a long history would only cost memory.
        if self.history.len() > 100 {
            self.history.remove(0);
        }
    }

    pub fn undo(&mut self) {
        if let Some((t, c, a)) = self.history.pop() {
            self.text = t;
            self.cursor = c;
            self.anchor = a;
        }
        self.typing = false;
    }

    /// Put `s` in place of the selection, or at the cursor.
    pub fn insert(&mut self, s: &str) {
        let one_char = s.chars().count() == 1;
        if !(one_char && self.typing && !self.has_selection()) {
            self.remember();
        }
        let (a, b) = self.selection();
        self.text.replace_range(a..b, s);
        self.cursor = a + s.len();
        self.anchor = self.cursor;
        self.typing = one_char;
    }

    /// Remove the selection; true when there was one.
    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        self.remember();
        let (a, b) = self.selection();
        self.text.replace_range(a..b, "");
        self.cursor = a;
        self.anchor = a;
        true
    }

    /// Backspace: the selection, else one character (or word) back.
    pub fn backspace(&mut self, word: bool) {
        self.typing = false;
        if self.delete_selection() || self.cursor == 0 {
            return;
        }
        let from = if word {
            word_left(&self.text, self.cursor)
        } else {
            prev(&self.text, self.cursor)
        };
        self.remember();
        self.text.replace_range(from..self.cursor, "");
        self.cursor = from;
        self.anchor = from;
    }

    /// Delete: the selection, else one character (or word) forward.
    pub fn delete(&mut self, word: bool) {
        self.typing = false;
        if self.delete_selection() || self.cursor == self.text.len() {
            return;
        }
        let to = if word {
            word_right(&self.text, self.cursor)
        } else {
            next(&self.text, self.cursor)
        };
        self.remember();
        self.text.replace_range(self.cursor..to, "");
        self.anchor = self.cursor;
    }

    /// Move the cursor to `pos`; with `extend` the selection follows it.
    pub fn move_to(&mut self, pos: usize, extend: bool) {
        self.typing = false;
        self.cursor = floor(&self.text, pos.min(self.text.len()));
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn left(&mut self, extend: bool, word: bool) {
        // Without Shift, Left on a selection goes to its start.
        if self.has_selection() && !extend && !word {
            let (a, _) = self.selection();
            return self.move_to(a, false);
        }
        let to = if word {
            word_left(&self.text, self.cursor)
        } else {
            prev(&self.text, self.cursor)
        };
        self.move_to(to, extend);
    }

    pub fn right(&mut self, extend: bool, word: bool) {
        if self.has_selection() && !extend && !word {
            let (_, b) = self.selection();
            return self.move_to(b, false);
        }
        let to = if word {
            word_right(&self.text, self.cursor)
        } else {
            next(&self.text, self.cursor)
        };
        self.move_to(to, extend);
    }

    pub fn select_all(&mut self) {
        self.typing = false;
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// Double-click: the word (or run of other characters) that starts at
    /// or covers `pos`, the character under the pointer.
    pub fn select_word_at(&mut self, pos: usize) {
        self.typing = false;
        let pos = floor(&self.text, pos.min(self.text.len()));
        let kind = |c: char| (is_word(c), c.is_whitespace());
        let here = self.text[pos..]
            .chars()
            .next()
            .or_else(|| self.text[..pos].chars().next_back());
        let Some(k) = here.map(kind) else {
            return;
        };
        let start = self.text[..pos]
            .char_indices()
            .rev()
            .take_while(|(_, c)| kind(*c) == k)
            .last()
            .map_or(pos, |(i, _)| i);
        let end = self.text[pos..]
            .char_indices()
            .take_while(|(_, c)| kind(*c) == k)
            .last()
            .map_or(pos, |(i, c)| pos + i + c.len_utf8());
        self.anchor = start;
        self.cursor = end;
    }

    /// Ctrl+X: the selection, taken out of the field.
    pub fn cut(&mut self) -> Option<String> {
        let s = self.selected().to_string();
        (!s.is_empty() && self.delete_selection()).then_some(s)
    }

    /// The common keys of a text field. `ch` is the character the key types.
    pub fn key(&mut self, sym: u32, ctrl: bool, shift: bool, ch: Option<char>) -> Key {
        // keysyms: X11/keysymdef.h; the keypad ones are the same keys.
        match sym {
            0xff1b => return Key::Cancel,
            0xff0d | 0xff8d => return Key::Commit,
            0xff09 => return Key::Next(false),
            0xfe20 => return Key::Next(true),
            0xff08 => self.backspace(ctrl),
            0xffff | 0xff9f => self.delete(ctrl),
            0xff51 | 0xff96 => self.left(shift, ctrl),
            0xff53 | 0xff98 => self.right(shift, ctrl),
            // Home and End, and Up and Down as in a one-line field.
            0xff50 | 0xff95 | 0xff52 | 0xff97 => self.move_to(0, shift),
            0xff57 | 0xff9c | 0xff54 | 0xff99 => self.move_to(self.text.len(), shift),
            0x61 | 0x41 if ctrl => self.select_all(),
            0x63 | 0x43 if ctrl => return Key::Copy,
            0x78 | 0x58 if ctrl => return Key::Cut,
            0x76 | 0x56 if ctrl => return Key::Paste,
            0x7a | 0x5a if ctrl => self.undo(),
            _ if ctrl => return Key::Ignored,
            _ => match ch {
                Some(c) if !c.is_control() => self.insert(c.encode_utf8(&mut [0; 4])),
                _ => return Key::Ignored,
            },
        }
        Key::Edited
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

fn floor(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn prev(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map_or(0, |(k, _)| k)
}

fn next(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

/// Ctrl+Left: over separators, then to the start of the word.
fn word_left(s: &str, i: usize) -> usize {
    let mut k = i;
    let mut seen_word = false;
    for (j, c) in s[..i].char_indices().rev() {
        if is_word(c) {
            seen_word = true;
        } else if seen_word {
            break;
        }
        k = j;
    }
    k
}

/// Ctrl+Right: over separators, then to the end of the word.
fn word_right(s: &str, i: usize) -> usize {
    let mut seen_word = false;
    for (j, c) in s[i..].char_indices() {
        if is_word(c) {
            seen_word = true;
        } else if seen_word {
            return i + j;
        }
    }
    s.len()
}

/// Where the part of a name to select first ends: before the extension of
/// a file ("photo" of "photo.jpg", "backup" of "backup.tar.gz"), the whole
/// name of a folder or of a hidden file with no other dot.
pub fn stem_end(name: &str, is_dir: bool) -> usize {
    if is_dir {
        return name.len();
    }
    let Some(dot) = name.rfind('.').filter(|d| *d > 0 && *d + 1 < name.len()) else {
        return name.len();
    };
    // Compressed tar archives keep ".tar" as part of the extension.
    if name[..dot].to_ascii_lowercase().ends_with(".tar") && dot > 4 {
        return dot - 4;
    }
    dot
}

/// The name split as `stem_end` sees it: ("photo", ".jpg").
pub fn split_name(name: &str, is_dir: bool) -> (&str, &str) {
    name.split_at(stem_end(name, is_dir))
}

/// Why `name` cannot be a file name, in words for the status line.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("A name cannot be empty".into());
    }
    if name == "." || name == ".." {
        return Err(format!("\"{name}\" is not allowed as a name"));
    }
    if name.contains('/') {
        return Err("A name cannot contain \"/\"".into());
    }
    if name.contains('\0') {
        return Err("A name cannot contain a null character".into());
    }
    // NAME_MAX on Linux file systems is 255 bytes, not characters.
    if name.len() > 255 {
        return Err(format!(
            "The name is too long ({} bytes, at most 255)",
            name.len()
        ));
    }
    Ok(())
}

/// A pasted text made fit for a one-line name: lines joined by spaces,
/// and with `no_slash` any "/" left out.
pub fn one_line(s: &str, no_slash: bool) -> String {
    s.trim_matches(|c: char| c == '\n' || c == '\r')
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !c.is_control() && !(no_slash && *c == '/'))
        .collect()
}

/// How a batch rename changes the letters of the new names (before the
/// extension).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Case {
    #[default]
    Keep,
    Lower,
    Upper,
    Title,
}

impl Case {
    pub fn next(self) -> Case {
        match self {
            Case::Keep => Case::Lower,
            Case::Lower => Case::Upper,
            Case::Upper => Case::Title,
            Case::Title => Case::Keep,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Case::Keep => "as typed",
            Case::Lower => "lower case",
            Case::Upper => "UPPER CASE",
            Case::Title => "Title Case",
        }
    }
    fn apply(self, s: &str) -> String {
        match self {
            Case::Keep => s.to_string(),
            Case::Lower => s.to_lowercase(),
            Case::Upper => s.to_uppercase(),
            Case::Title => {
                let mut out = String::with_capacity(s.len());
                let mut start = true;
                for c in s.chars() {
                    if start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    start = c.is_whitespace() || matches!(c, '_' | '-' | '.' | '(' | '[');
                }
                out
            }
        }
    }
}

/// What a batch rename does to each name.
#[derive(Clone, Debug, Default)]
pub struct Batch {
    /// Empty: the whole name before the extension is replaced.
    pub find: String,
    /// With `{n}`, `{n:03}`, `{name}`, `{ext}` and `{date}`; with `regex`
    /// also `$1` for what a group of `find` matched.
    pub replace: String,
    pub regex: bool,
    pub case: Case,
}

/// One item of a batch rename: its name, whether it is a folder, and its
/// modification time (for `{date}`).
pub struct BatchItem {
    pub name: String,
    pub is_dir: bool,
    pub mtime: i64,
}

/// `{n}`, `{n:03}`, `{name}`, `{ext}` and `{date}` in `pattern`. Unknown
/// braces stay as they are. `dollar` doubles "$" in what is put in, for a
/// regex replacement.
fn tokens(
    pattern: &str,
    number: usize,
    width: usize,
    item: &BatchItem,
    stem: &str,
    ext: &str,
    dollar: bool,
) -> String {
    let mut out = String::with_capacity(pattern.len() + stem.len());
    let mut rest = pattern;
    let put = |out: &mut String, s: &str| {
        if dollar {
            out.push_str(&s.replace('$', "$$"));
        } else {
            out.push_str(s);
        }
    };
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let tok = &after[..close];
        match tok {
            "n" => out.push_str(&format!("{number:0width$}")),
            "name" => put(&mut out, stem),
            "ext" => put(&mut out, ext.trim_start_matches('.')),
            "date" => out.push_str(&crate::dir::human_time(item.mtime)[..10]),
            _ => match tok.strip_prefix("n:").and_then(|w| w.parse::<usize>().ok()) {
                Some(w) => out.push_str(&format!("{number:0w$}", w = w.min(12))),
                None => {
                    out.push('{');
                    out.push_str(tok);
                    out.push('}');
                }
            },
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// New names for a batch rename, or why the pattern is wrong (a regex
/// that does not parse). The number counts from `first` over the items
/// that change, padded to the same width for all (at least two digits)
/// unless `{n:WIDTH}` says otherwise. Folders keep their whole name as
/// the stem, and extensions stay as they are.
pub fn batch_names(items: &[BatchItem], b: &Batch, first: usize) -> Result<Vec<String>, String> {
    let re =
        if b.regex && !b.find.is_empty() {
            Some(regex_lite::Regex::new(&b.find).map_err(|e| {
                format!("The pattern does not work: {}", first_line(&e.to_string()))
            })?)
        } else {
            None
        };
    let hit = |it: &BatchItem| {
        let stem = split_name(&it.name, it.is_dir).0;
        match &re {
            _ if b.find.is_empty() => true,
            Some(re) => re.is_match(stem),
            None => stem.contains(b.find.as_str()),
        }
    };
    let hits = items.iter().filter(|it| hit(it)).count();
    let width = (first + hits.saturating_sub(1)).to_string().len().max(2);
    let mut number = first;
    Ok(items
        .iter()
        .map(|it| {
            if !hit(it) {
                return it.name.clone();
            }
            let (stem, ext) = split_name(&it.name, it.is_dir);
            let with = tokens(&b.replace, number, width, it, stem, ext, re.is_some());
            number += 1;
            let new = if b.find.is_empty() {
                if b.replace.is_empty() {
                    stem.to_string()
                } else {
                    with
                }
            } else if let Some(re) = &re {
                re.replace_all(stem, with.as_str()).into_owned()
            } else {
                stem.replace(b.find.as_str(), &with)
            };
            format!("{}{ext}", b.case.apply(&new))
        })
        .collect())
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(e: &mut LineEdit, s: &str) {
        for c in s.chars() {
            e.key(c as u32, false, false, Some(c));
        }
    }

    #[test]
    fn stem_is_selected_first() {
        assert_eq!(stem_end("photo.jpg", false), 5);
        assert_eq!(stem_end("backup.tar.gz", false), 6);
        assert_eq!(stem_end("x.TAR.XZ", false), 1);
        assert_eq!(stem_end(".bashrc", false), 7);
        assert_eq!(stem_end(".config.toml", false), 7);
        assert_eq!(stem_end("README", false), 6);
        assert_eq!(stem_end("ends.", false), 5);
        assert_eq!(stem_end("my.folder", true), 9);
        assert_eq!(stem_end(".tar.gz", false), 4);
        let e = LineEdit::selecting("photo.jpg", 0, 5);
        assert_eq!(e.selected(), "photo");
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut e = LineEdit::selecting("photo.jpg", 0, 5);
        typed(&mut e, "beach");
        assert_eq!(e.text, "beach.jpg");
        assert_eq!(e.cursor, 5);
        assert!(!e.has_selection());
    }

    #[test]
    fn cursor_keys_and_word_jumps() {
        let mut e = LineEdit::new("my holiday-photo.jpg");
        e.key(0xff50, false, false, None); // Home
        assert_eq!(e.cursor, 0);
        e.key(0xff53, true, false, None); // Ctrl+Right
        assert_eq!(e.cursor, 2);
        e.key(0xff53, true, false, None);
        assert_eq!(e.cursor, 10);
        e.key(0xff53, true, true, None); // Ctrl+Shift+Right
        assert_eq!(e.selected(), "-photo");
        e.key(0xff51, false, false, None); // Left: to the selection's start
        assert_eq!(e.cursor, 10);
        e.key(0xff57, false, false, None); // End
        e.key(0xff51, true, false, None); // Ctrl+Left
        assert_eq!(e.cursor, 17);
        e.key(0xff51, true, false, None);
        assert_eq!(e.cursor, 11);
        e.key(0xff51, false, true, None); // Shift+Left
        assert_eq!(e.selected(), "-");
    }

    #[test]
    fn deleting() {
        let mut e = LineEdit::new("one two.txt");
        e.move_to(7, false);
        e.key(0xff08, true, false, None); // Ctrl+Backspace: a word
        assert_eq!(e.text, "one .txt");
        e.key(0xff08, false, false, None);
        assert_eq!(e.text, "one.txt");
        e.key(0xffff, false, false, None); // Delete
        assert_eq!(e.text, "onetxt");
        e.key(0xff50, false, false, None);
        e.key(0xffff, true, false, None); // Ctrl+Delete: a word
        assert_eq!(e.text, "");
        // Ctrl+Backspace with everything selected empties the field.
        let mut e = LineEdit::selecting("photos", 0, 6);
        e.key(0xff08, true, false, None);
        assert_eq!(e.text, "");
    }

    #[test]
    fn multibyte_characters_stay_whole() {
        let mut e = LineEdit::new("sång å.txt");
        e.move_to(2, false); // inside "å": falls back to its start
        assert_eq!(e.cursor, 1);
        e.key(0xff53, false, false, None);
        assert_eq!(e.cursor, 3);
        e.key(0xff08, false, false, None);
        assert_eq!(e.text, "sng å.txt");
        e.key(0xff53, true, false, None);
        assert_eq!(&e.text[..e.cursor], "sng");
        typed(&mut e, "ö");
        assert_eq!(e.text, "sngö å.txt");
    }

    #[test]
    fn select_all_cut_and_undo() {
        let mut e = LineEdit::new("name.txt");
        assert_eq!(e.key(0x61, true, false, None), Key::Edited);
        assert_eq!(e.selected(), "name.txt");
        assert_eq!(e.key(0x78, true, false, None), Key::Cut);
        assert_eq!(e.cut().as_deref(), Some("name.txt"));
        assert_eq!(e.text, "");
        typed(&mut e, "new");
        assert_eq!(e.text, "new");
        // Typing is one undo step, the cut another.
        e.key(0x7a, true, false, None);
        assert_eq!(e.text, "");
        e.key(0x7a, true, false, None);
        assert_eq!(e.text, "name.txt");
        assert_eq!(e.key(0xff0d, false, false, None), Key::Commit);
        assert_eq!(e.key(0xff1b, false, false, None), Key::Cancel);
        assert_eq!(e.key(0x76, true, false, None), Key::Paste);
        assert_eq!(e.key(0x71, true, false, None), Key::Ignored);
    }

    #[test]
    fn double_click_selects_a_word() {
        let mut e = LineEdit::new("my holiday-photo.jpg");
        e.select_word_at(5);
        assert_eq!(e.selected(), "holiday");
        e.select_word_at(10);
        assert_eq!(e.selected(), "-");
        e.select_word_at(20);
        assert_eq!(e.selected(), "jpg");
        e.select_word_at(2);
        assert_eq!(e.selected(), " ");
        e.select_word_at(11);
        assert_eq!(e.selected(), "photo");
    }

    #[test]
    fn names_are_checked() {
        assert!(check_name("ok.txt").is_ok());
        assert!(check_name(" lead").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("   ").is_err());
        assert!(check_name(".").is_err());
        assert!(check_name("..").is_err());
        assert!(check_name("a/b").unwrap_err().contains('/'));
        assert!(check_name("a\0b").is_err());
        assert!(check_name(&"x".repeat(255)).is_ok());
        assert!(check_name(&"x".repeat(256)).is_err());
        // 128 two-byte characters are 256 bytes.
        assert!(check_name(&"å".repeat(128)).is_err());
    }

    #[test]
    fn pasted_text_is_one_line() {
        assert_eq!(one_line("a/b\nc\n", true), "ab c");
        assert_eq!(one_line("x\ty", false), "x y");
        assert_eq!(one_line("/tmp/x", false), "/tmp/x");
    }

    fn items(v: &[(&str, bool)]) -> Vec<BatchItem> {
        v.iter()
            .map(|(n, d)| BatchItem {
                name: n.to_string(),
                is_dir: *d,
                mtime: 0,
            })
            .collect()
    }

    fn names(items: &[BatchItem], find: &str, replace: &str, first: usize) -> Vec<String> {
        let b = Batch {
            find: find.into(),
            replace: replace.into(),
            ..Batch::default()
        };
        batch_names(items, &b, first).unwrap()
    }

    #[test]
    fn batch_names_find_replace_and_number() {
        let it = items(&[
            ("IMG_001.jpg", false),
            ("IMG_002.JPG", false),
            ("IMG dir", true),
        ]);
        assert_eq!(names(&it, "IMG_", "", 1), ["001.jpg", "002.JPG", "IMG dir"]);
        assert_eq!(
            names(&it, "", "Summer {n}", 1),
            ["Summer 01.jpg", "Summer 02.JPG", "Summer 03"]
        );
        assert_eq!(
            names(&it, "IMG", "{n}", 9),
            ["09_001.jpg", "10_002.JPG", "11 dir"]
        );
        assert_eq!(names(&it[..1], "", "{name} copy", 1), ["IMG_001 copy.jpg"]);
        // Only the names that change are counted.
        let it = items(&[("a.txt", false), ("IMG_9.jpg", false), ("IMG_7.jpg", false)]);
        assert_eq!(
            names(&it, "IMG_", "Trip {n} ", 1),
            ["a.txt", "Trip 01 9.jpg", "Trip 02 7.jpg"]
        );
    }

    #[test]
    fn batch_pattern_tokens() {
        let mut it = items(&[
            ("beach.jpg", false),
            ("backup.tar.gz", false),
            ("Dir", true),
        ]);
        it[0].mtime = 1_700_000_000;
        let d = crate::dir::human_time(1_700_000_000)[..10].to_string();
        assert_eq!(
            names(&it, "", "Holiday {n:03}", 1),
            ["Holiday 001.jpg", "Holiday 002.tar.gz", "Holiday 003"]
        );
        assert_eq!(
            names(&it[..2], "", "{name}-{ext}", 1),
            ["beach-jpg.jpg", "backup-tar.gz.tar.gz"]
        );
        assert_eq!(
            names(&it[..1], "", "{date} {name}", 1),
            [format!("{d} beach.jpg")]
        );
        // Unknown braces stay, and so does a lone one.
        assert_eq!(names(&it[..1], "", "{x} {n:zz} {", 1), ["{x} {n:zz} {.jpg"]);
        // Nothing to find and nothing to put: the names stay.
        assert_eq!(names(&it, "", "", 1), ["beach.jpg", "backup.tar.gz", "Dir"]);
    }

    #[test]
    fn batch_regex_and_case() {
        let it = items(&[
            ("IMG_2024_07.jpg", false),
            ("notes.TXT", false),
            ("my holiday-pics", true),
        ]);
        let mut b = Batch {
            find: r"IMG_(\d+)_(\d+)".into(),
            replace: "$2-$1 {name}".into(),
            regex: true,
            ..Batch::default()
        };
        assert_eq!(
            batch_names(&it, &b, 1).unwrap(),
            ["07-2024 IMG_2024_07.jpg", "notes.TXT", "my holiday-pics"]
        );
        // A "$" in an old name is put in as it is.
        let it2 = items(&[("IMG_1_2 $1.jpg", false)]);
        b.replace = "{name}".into();
        assert_eq!(batch_names(&it2, &b, 1).unwrap(), ["IMG_1_2 $1 $1.jpg"]);
        b.find = "(".into();
        assert!(batch_names(&it, &b, 1)
            .unwrap_err()
            .starts_with("The pattern does not work"));
        let mut b = Batch {
            case: Case::Upper,
            ..Batch::default()
        };
        assert_eq!(
            batch_names(&it, &b, 1).unwrap(),
            ["IMG_2024_07.jpg", "NOTES.TXT", "MY HOLIDAY-PICS"]
        );
        b.case = Case::Title;
        assert_eq!(
            batch_names(&it, &b, 1).unwrap(),
            ["Img_2024_07.jpg", "Notes.TXT", "My Holiday-Pics"]
        );
        b.case = Case::Lower;
        b.find = "o".into();
        b.replace = "Ö".into();
        assert_eq!(
            batch_names(&it, &b, 1).unwrap(),
            ["IMG_2024_07.jpg", "nötes.TXT", "my höliday-pics"]
        );
        assert_eq!(Case::Title.next(), Case::Keep);
    }
}
