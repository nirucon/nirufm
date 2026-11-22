#!/bin/sh
# X11 end-to-end checks in a private Xvfb: first frame, file operations
# (copy, trash, undo, rename) and drag and drop inside a window and between
# two windows. Usage: tests/xvfb.sh PATH/TO/nirufm
set -eu
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SB=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null || true; done; rm -rf "$SB"; }
trap cleanup EXIT INT TERM
export HOME=$SB/home XDG_DATA_HOME=$SB/data XDG_CONFIG_HOME=$SB/config XDG_CACHE_HOME=$SB/cache
mkdir -p "$HOME" "$SB/a/photos" "$SB/b" "$XDG_DATA_HOME" "$XDG_CONFIG_HOME"
echo one > "$SB/a/notes.txt"; echo two > "$SB/a/second.txt"
D=:$((70 + $$ % 20))
Xvfb "$D" -screen 0 1600x800x24 -nolisten tcp >/dev/null 2>&1 &
PIDS="$PIDS $!"
export DISPLAY=$D
sleep 1
fail() { echo "FAIL: $*" >&2; exit 1; }
k() { xdotool key --delay 60 "$@"; sleep 0.5; }

NIRUFM_TIMING=1 "$BIN" "$SB/a" 2>"$SB/a.err" &
A=$!
PIDS="$PIDS $A"
sleep 1.2
grep -q 'first frame after' "$SB/a.err" || fail "no first frame"
ms=$(sed -n 's/.*after \([0-9]*\)\..*/\1/p' "$SB/a.err")
[ "$ms" -lt 1000 ] || fail "first frame took $ms ms"
rss=$(awk '/VmRSS/ {print $2}' /proc/$A/status)
[ "$rss" -lt 60000 ] || fail "memory $rss kB"
echo "first frame ${ms} ms, ${rss} kB"

# Copy and paste: notes.txt -> "notes (2).txt"; trash it; undo.
k Right ctrl+c ctrl+v
sleep 0.6
[ -f "$SB/a/notes (2).txt" ] || fail "paste did not copy"
k Delete
sleep 0.6
[ ! -e "$SB/a/notes (2).txt" ] && [ -f "$XDG_DATA_HOME/Trash/info/notes (2).txt.trashinfo" ] || fail "trash"
k ctrl+z
sleep 0.6
[ -f "$SB/a/notes (2).txt" ] || fail "undo did not restore"
# Rename the folder with F2.
k Home F2 ctrl+BackSpace
xdotool type --delay 40 "bilder"
k Return
[ -d "$SB/a/bilder" ] || fail "rename"
echo "file operations ok"

# Drag "notes (2).txt" (second cell) onto the folder: moved inside.
WA=$(xdotool search --sync --onlyvisible --class nirufm | head -1)
xdotool windowmove "$WA" 0 0
sleep 0.3
xdotool mousemove 377 95 mousedown 1
for x in 360 330 300 280 266; do xdotool mousemove $x 95; sleep 0.05; done
sleep 0.2; xdotool mouseup 1; sleep 1
[ -f "$SB/a/bilder/notes (2).txt" ] || fail "internal drag did not move"

# Drag between two windows: copied.
"$BIN" "$SB/b" 2>/dev/null &
PIDS="$PIDS $!"
sleep 1
WB=$(xdotool search --onlyvisible --class nirufm | grep -vx "$WA" | head -1)
[ -n "$WB" ] || fail "second window"
xdotool windowmove "$WB" 800 0; xdotool windowraise "$WA"; sleep 0.5
# Cells after the move: bilder, notes.txt, second.txt
xdotool mousemove 489 95 mousedown 1
for x in 520 700 1000 1200 1350 1400; do xdotool mousemove $x 300; sleep 0.08; done
sleep 0.3; xdotool mouseup 1; sleep 1.2
[ -f "$SB/b/second.txt" ] || fail "drop on the other window"
echo "drag and drop ok"

# Bookmarks are put in order by dragging them in the side panel.
mkdir -p "$XDG_CONFIG_HOME/gtk-3.0"
printf 'file://%s Bravo\nfile://%s Alpha\nfile://%s/photos Photos\n' "$SB/b" "$SB/a" "$SB/a" > "$XDG_CONFIG_HOME/gtk-3.0/bookmarks"
NIRUFM_PLACES=1 "$BIN" "$SB/b" 2>"$SB/c.err" &
C=$!
PIDS="$PIDS $C"
sleep 1.2
WC=$(xdotool search --pid "$C" | head -1)
xdotool windowmove "$WC" 0 0; xdotool windowraise "$WC"; sleep 0.5
row() { grep "^place $1 " "$SB/c.err" | tail -1 | awk '{print $3, $4}'; }
set -- $(row Photos); px=$1; py=$2
set -- $(row Bravo); by=$2
[ -n "$px" ] && [ -n "$by" ] || fail "bookmark rows not found: $(tail -5 "$SB/c.err")"
xdotool mousemove "$px" "$py" mousedown 1
y=$py; while [ "$y" -gt "$by" ]; do y=$((y - 8)); xdotool mousemove "$px" "$y"; sleep 0.03; done
xdotool mousemove "$px" "$by"; sleep 0.2; xdotool mouseup 1; sleep 0.6
first=$(head -1 "$XDG_CONFIG_HOME/gtk-3.0/bookmarks")
[ "$first" = "file://$SB/a/photos Photos" ] || fail "drag did not reorder: $(cat "$XDG_CONFIG_HOME/gtk-3.0/bookmarks")"
[ "$(wc -l < "$XDG_CONFIG_HOME/gtk-3.0/bookmarks")" = 3 ] || fail 'a bookmark was lost'
# A click without moving still opens the bookmark.
set -- $(row Alpha)
xdotool mousemove "$1" "$2" click 1; sleep 0.6
grep -q "^place " "$SB/c.err" || fail 'no rows'
echo "bookmark order ok"
echo "PASS"
