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

/// New names for a batch rename. `find` empty replaces the whole name
/// before the extension; `{n}` in `replace` is a running number counted
/// from `first` over the items that change, padded to the same width for
/// all, and `{name}` the old name before the extension. Folders keep their
/// whole name as the stem.
pub fn batch_names(
    items: &[(String, bool)],
    find: &str,
    replace: &str,
    first: usize,
) -> Vec<String> {
    let hit =
        |name: &String, is_dir: bool| find.is_empty() || split_name(name, is_dir).0.contains(find);
    let hits = items.iter().filter(|(n, d)| hit(n, *d)).count();
    let width = (first + hits.saturating_sub(1)).to_string().len().max(2);
    let mut number = first;
    items
        .iter()
        .map(|(name, is_dir)| {
            if !hit(name, *is_dir) {
                return name.clone();
            }
            let (stem, ext) = split_name(name, *is_dir);
            let with = replace
                .replace("{n}", &format!("{number:0width$}"))
                .replace("{name}", stem);
            number += 1;
            let stem = if find.is_empty() {
                with
            } else {
                stem.replace(find, &with)
            };
            format!("{stem}{ext}")
        })
        .collect()
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

    #[test]
    fn batch_names_find_replace_and_number() {
        let items = vec![
            ("IMG_001.jpg".to_string(), false),
            ("IMG_002.JPG".to_string(), false),
            ("IMG dir".to_string(), true),
        ];
        assert_eq!(
            batch_names(&items, "IMG_", "", 1),
            ["001.jpg", "002.JPG", "IMG dir"]
        );
        assert_eq!(
            batch_names(&items, "", "Summer {n}", 1),
            ["Summer 01.jpg", "Summer 02.JPG", "Summer 03"]
        );
        assert_eq!(
            batch_names(&items, "IMG", "{n}", 9),
            ["09_001.jpg", "10_002.JPG", "11 dir"]
        );
        assert_eq!(
            batch_names(&items[..1], "", "{name} copy", 1),
            ["IMG_001 copy.jpg"]
        );
        // Only the names that change are counted.
        let items = vec![
            ("a.txt".to_string(), false),
            ("IMG_9.jpg".to_string(), false),
            ("IMG_7.jpg".to_string(), false),
        ];
        assert_eq!(
            batch_names(&items, "IMG_", "Trip {n} ", 1),
            ["a.txt", "Trip 01 9.jpg", "Trip 02 7.jpg"]
        );
    }
}
