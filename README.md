# NIRUFM

The file manager of the NIRU suite: fast, light and drawn in the NIRU theme.

**Status: beta** · Part of the [NIRUX](https://github.com/nirucon/nirux) suite · [MIT License](LICENSE)

## About

NIRUFM opens in a few hundredths of a second and uses around 15 MB of memory,
so it can be opened and closed as often as a terminal. It draws everything
itself on X11, like NIRUWM and NIRULAUNCHER, so it follows the NIRU theme
exactly and changes with it live, with pixel touches in the NIRUX style.

| | NIRUFM | PCManFM 1.3 |
|---|---|---|
| Window shown (home folder) | 61 ms | 299 ms |
| Memory | 14 MB | 102 MB |

Measured on the same machine in the same X server (Debian 13). A folder of
8,365 files: NIRUFM 67 ms and 16 MB.

## Features

- **Four views:** icons, thumbnails, compact and a detailed list, switched
  with the buttons at the top right or Ctrl+1 to Ctrl+4. Each folder remembers
  its view, zoom and sort; a folder of photos opens as thumbnails.
- **Gallery** for photos and films: film strip, slideshow, full screen, zoom
  and pan, turning, EXIF facts, "set as wallpaper", trash. Enter on a photo or
  Ctrl+G.
- **NIRUX icons:** retro pixel icons from
  [NIRUICONS](https://github.com/nirucon/niruicons) for folders, places,
  drives and the common file types, in the colours of the NIRU theme. A theme
  change recolours them at once. Types the set has no picture for take the
  icon theme's (Papirus) icon in grey. `icons = "theme"` in
  `~/.config/nirufm/config.toml` uses the icon theme alone, in its own
  colours.
- **Preview** with Space: pictures, the first page of a PDF, text.
- **Thumbnails** through the freedesktop cache and the system's thumbnailers,
  shared with every other program.
- **File operations** on a background thread with progress: copy, move,
  trash (freedesktop trash, restorable), delete, undo, redo, rename. A copy
  is written to a partial file first and nothing is overwritten unasked.
  Ctrl+Z undoes renames, moves, copies, trashing and new items, and
  Ctrl+Shift+Z (or Ctrl+Y) redoes them until something new is done.
- **Names that exist** when pasting, moving or dropping: you choose Replace,
  Keep both (the new one gets " (2)"), Skip or, for a folder onto a folder,
  Merge (asking again for names inside), with "apply to all remaining" for
  the rest. A replaced item goes to the trash only after the new one is
  complete, and Ctrl+Z brings it back.
- **Rename** in place with F2: the name before the extension is selected,
  the field works like any text field (cursor keys, Ctrl+Left and
  Ctrl+Right, Shift to select, a click places the cursor, a double-click
  takes a word, Ctrl+C, Ctrl+X, Ctrl+V and Ctrl+Z) and Tab goes on to the
  next name. A name that exists, is empty or has "/" in it is refused and
  the field stays open; nothing is ever replaced and Ctrl+Z undoes it. The
  path bar (Ctrl+L), search (Ctrl+F) and the Connect form are the same kind
  of field.
- **Batch rename** with F2 on several items: a pattern such as
  `Holiday {n:03}` for the whole name, or find and replace in the names
  (Alt+R makes the find a regex, with `$1` for its groups). `{n}` is a
  running number (`{n:03}` with three digits), `{name}` the old name,
  `{ext}` its extension and `{date}` its date. Alt+C changes the case:
  lower, UPPER or Title. A preview shows every old and new name and marks
  the ones that clash with a name that exists or with each other; nothing
  is replaced, and one Ctrl+Z puts all the names back.
- **Type to filter:** typing in a folder keeps only the names that hold
  what you typed, whatever the case or accents ("cafe" finds "Café").
  Backspace takes a letter back, Esc shows everything again, and `/`
  starts typing a path. Enter, F2 and the other keys work on what is left.
- **New items** from the menu on blank space: a folder (Ctrl+Shift+N), an
  empty file (Ctrl+Alt+N) or a copy of any file in `~/Templates` (or the
  folder `XDG_TEMPLATES_DIR` names in `user-dirs.dirs`). The new item opens
  in renaming with its name selected.
- **Clipboard** shared with other file managers, and **drag and drop** in and
  out (XDND), onto folders, places, tabs and the other pane. A label beside
  the pointer says how many files and whether they move or copy, a folder
  held under the pointer opens so the drop can go deeper, the folder scrolls
  when the pointer nears its edge, and Esc cancels.
- **Select with the mouse:** drag a band from blank space, or from beside a
  name, over the files (in the list view it takes whole rows). Ctrl adds to
  the selection, Shift flips what the band covers, Esc puts it back; the
  folder scrolls on when the band reaches its edge.
- **Tabs and two panes** (F3), **search** below a folder (Ctrl+F) and a
  **properties** sheet (Alt+Enter).
- **Compare two panes** with Shift+F3: items only on one side and files
  that differ in size or time get a mark ("+" only here, an arrow on the
  newer copy), and what the other side lacks is selected, ready for F6,
  which copies the selection to the other pane (asking for names that
  exist).
- **Folder size on demand:** Alt+S (or "Folder size" in the menu, or
  "Count size" in Properties) counts a folder on a thread of its own,
  showing the total as it grows; C or the button stops it, and closing the
  sheet does too. Nothing is counted unless you ask.
- **SHA-256** of a file in Properties: S or the button computes it in the
  background with its progress, and Ctrl+C or "Copy checksum" copies it.
- **Permissions** in the properties sheet: boxes for who may read, write
  and run (for a folder: open) the item, applied at once, and X to make a
  file executable in one go. When a change is not possible it says why in
  plain words.
- **Pack and unpack** zip, tar.gz, 7z and more.
- **Drives:** USB sticks and SD cards mount with a click and eject with ⏏
  (udisks2).
- **Servers:** sftp, ftp, smb and dav through gvfs (Ctrl+K). SSH keys work as
  in a terminal; hosts from `~/.ssh/config` complete with Tab.
- **Nextcloud:** copy the public or internal link, share, open in the
  browser, and sync badges on items, all through the desktop client.
- **Bookmarks** (the GTK list, shared with other programs): Ctrl+D or "Add to
  bookmarks" on folders; drag them in the side panel to put them in order;
  right-click to move up or down, rename, open in a new tab or remove.
- **Open with** any program for the file type: the menu lists the first
  six, and "Open with…" all of them, with "Always open … files with it" to
  make the choice the default (written to `~/.config/mimeapps.list`, the
  rest of that file kept).
- **Recent** in the side panel: the files and folders other programs
  opened lately (`recently-used.xbel`), read only when you open it.
- **Send to** in the menu: a NIRUSHARE device or all of them, Proton Drive
  (when `niruprodrive` is installed: send the files or copy a public link)
  and a new mail with the files attached (`xdg-email`, or Thunderbird).
  Each entry shows only when its program is there.
- Also a sound when a long copy ends, and the key sheet on F1.

## Using it

```text
nirufm [FOLDER]
```

Press F1 in the window for every key. The most used:

| Keys | |
|---|---|
| Enter, double-click | open (photos open in the gallery) |
| Backspace, Alt+Left, Alt+Right | parent, back, forward |
| Ctrl+C, Ctrl+X, Ctrl+V, Delete | copy, cut, paste, trash |
| Ctrl+Z, Ctrl+Shift+Z or Ctrl+Y | undo, redo |
| F2, Ctrl+Shift+N, Ctrl+Alt+N | rename (several: batch rename), new folder, new file |
| letters, Esc | filter the names, show all again |
| Space, Ctrl+G | preview, gallery |
| Ctrl+F, Ctrl+L | search, type a path |
| Alt+Enter, Alt+S | properties, folder size |
| Ctrl+T, F3 | new tab, two panes |
| Shift+F3, F6 | compare the panes, copy to the other pane |
| Ctrl+1 to Ctrl+4, Ctrl+wheel | views, zoom |

## Building

```sh
cargo build --release
sh tests/xvfb.sh target/release/nirufm   # end-to-end test in a private Xvfb
```

Needs Rust 1.82 or later. At run time: an X server, fontconfig (`fc-match`),
the shared MIME database and an icon theme for the types the NIRUX set does
not draw. Optional: gdk-pixbuf and
ffmpegthumbnailer (thumbnails), poppler-utils (PDF preview), udisks2 (drives),
gvfs with gvfs-fuse (servers), zip, unzip, 7z or bsdtar (archives). NIRUX
installs all of them.
