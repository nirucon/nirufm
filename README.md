# NIRUFM

A fast, light file manager for the NIRU suite.

**Status: prototype** · Part of the [NIRUX](https://github.com/nirucon/nirux) suite · [MIT License](LICENSE)

## About

NIRUFM is the everyday file manager of NIRUX: open a folder, find a file,
drag it somewhere, plug in a USB stick. It starts in well under a tenth of a
second and uses around 15 MB of memory, so it can be opened and closed as
often as a terminal.

It draws everything itself on X11, like NIRUWM and NIRULAUNCHER, so it
follows the NIRU theme exactly and changes with it live. Folder and file
icons come from the icon theme (Papirus in NIRUX).

The heavy jobs (servers, cloud remotes, comparing folders, resumable
transfers) belong to [NIRUORG](https://github.com/nirucon/niruorg). NIRUFM
stays small and quick.

| | NIRUFM | PCManFM 1.3 |
|---|---|---|
| Window shown (home folder) | 61 ms | 299 ms |
| Memory | 14 MB | 102 MB |

Measured on the same machine in the same X server (Debian 13, 8,365 files
for the large folder: NIRUFM 67 ms and 16 MB).

## Using it

```text
nirufm [FOLDER]
```

| Keys | |
|---|---|
| Enter, double-click | open |
| Backspace, Alt+Up | parent folder |
| Alt+Left, Alt+Right, mouse back/forward | history |
| Ctrl+T, Ctrl+W, Ctrl+Tab | new tab, close tab, next tab |
| Ctrl+L | type a path (Tab completes) |
| Ctrl+H | hidden files |
| Ctrl+1, Ctrl+2 | icons, list |
| Ctrl+A | select all |
| Ctrl+Shift+N | new folder |
| F4 | terminal here |
| F5 | reload |
| letters | jump to a name |

Middle-click a folder or a place to open it in a new tab, and a tab to
close it. Folders refresh by themselves when their content changes.

## Building

```sh
cargo build --release
```

Needs Rust 1.82 or later. At run time: an X server, fontconfig (`fc-match`),
the shared MIME database and an icon theme.

## Roadmap

- copy, move, rename, trash and undo, with progress
- drag and drop in and out
- thumbnails, and a preview for pictures and PDF
- packing and unpacking archives
- mounting and ejecting USB drives (udisks2)
- "Open with" and the context menu
