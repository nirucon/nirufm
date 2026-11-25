#!/bin/sh
# X11 end-to-end checks in a private Xvfb: first frame, file operations
# (copy, trash, undo, rename) and drag and drop inside a window and between
# two windows. Usage: tests/xvfb.sh PATH/TO/nirufm
set -eu
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SB=$(mktemp -d)
PIDS=""
cleanup() { for p in $PIDS; do kill "$p" 2>/dev/null || true; done; if [ -n "${KEEP:-}" ]; then echo "sandbox $SB"; else rm -rf "$SB"; fi; }
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
# A file: the name before ".txt" is selected, so typing replaces just that.
k Right F2
xdotool type --delay 40 "memo"
k Return
[ -f "$SB/a/memo.txt" ] && [ ! -e "$SB/a/notes (2).txt" ] || fail "rename keeps the extension"
k ctrl+z
[ -f "$SB/a/notes (2).txt" ] && [ ! -e "$SB/a/memo.txt" ] || fail "undo of a rename"
# Editing inside the name: End, Left over ".txt", Backspace over " (2)".
k F2 End Left Left Left Left BackSpace BackSpace BackSpace BackSpace
k minus
xdotool type --delay 40 "copy"
k Return
[ -f "$SB/a/notes-copy.txt" ] || fail "cursor keys in the rename field"
k ctrl+z
# A name that exists is refused and the field stays open to correct it.
k F2 ctrl+a
xdotool type --delay 40 "second.txt"
k Return ctrl+a
xdotool type --delay 40 "third.txt"
k Return
[ -f "$SB/a/third.txt" ] && [ "$(cat "$SB/a/second.txt")" = two ] || fail "rename onto an existing name"
k ctrl+z
[ -f "$SB/a/notes (2).txt" ] || fail "undo after a refused name"
# Several at once: find "notes", replace with a number.
k shift+Right F2
xdotool type --delay 40 "notes"
k Tab
xdotool type --delay 40 "{n} day"
k Return
[ -f "$SB/a/01 day (2).txt" ] && [ -f "$SB/a/02 day.txt" ] || fail "batch rename: $(ls "$SB/a")"
k ctrl+z Escape
[ -f "$SB/a/notes (2).txt" ] && [ -f "$SB/a/notes.txt" ] || fail "undo of a batch rename"
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

# Selecting with the mouse: a band from blank space over three files.
mkdir -p "$SB/c/deep"
for i in 1 2 3 4 5; do echo "$i" > "$SB/c/f$i.txt"; done
NIRUFM_PLACES=1 "$BIN" "$SB/c" 2>"$SB/band.err" &
E=$!
PIDS="$PIDS $E"
sleep 1.2
WE=$(xdotool search --pid "$E" | head -1)
xdotool windowmove "$WE" 0 0; xdotool windowraise "$WE"; sleep 0.5
item() { grep "^item $1 " "$SB/band.err" | tail -1 | awk '{print $3, $4, $5, $6}'; }
sel() { grep '^state ' "$SB/band.err" | tail -1 | awk '{print $2}'; }
# Cells: deep, f1.txt .. f5.txt. From below f1 to the middle of f3.
set -- $(item f1.txt); x1=$1; y1=$2; h1=$4
set -- $(item f3.txt); x3=$(($1 + $3 / 2)); y3=$(($2 + $4 / 2))
[ -n "$x1" ] && [ -n "$x3" ] || fail "items not found: $(tail -3 "$SB/band.err")"
xdotool mousemove $((x1 + 4)) $((y1 + h1 + 40)) mousedown 1
for t in 1 2 3 4 5; do xdotool mousemove $((x1 + 4 + (x3 - x1) * t / 5)) $((y1 + h1 + 40 - (y1 + h1 + 40 - y3) * t / 5)); sleep 0.05; done
sleep 0.2
[ "$(sel)" = 3 ] || fail "band selected $(sel), not 3"
# Esc while drawing puts the selection back (nothing).
xdotool key Escape; sleep 0.3; xdotool mouseup 1; sleep 0.3
[ "$(sel)" = 0 ] || fail "Esc left $(sel) selected"
# Draw it again and let go: three stay selected; Ctrl adds f5.
xdotool mousemove $((x1 + 4)) $((y1 + h1 + 40)) mousedown 1
for t in 1 2 3 4 5; do xdotool mousemove $((x1 + 4 + (x3 - x1) * t / 5)) $((y1 + h1 + 40 - (y1 + h1 + 40 - y3) * t / 5)); sleep 0.05; done
xdotool mouseup 1; sleep 0.3
[ "$(sel)" = 3 ] || fail "band kept $(sel), not 3"
set -- $(item f5.txt); x5=$(($1 + $3 / 2)); y5=$(($2 + $4 / 2)); b5=$(($2 + $4 + 30))
xdotool keydown ctrl mousemove "$x5" "$b5" mousedown 1
xdotool mousemove $((x5 + 20)) $((b5 - 10)); sleep 0.05; xdotool mousemove "$x5" "$y5"; sleep 0.2
xdotool mouseup 1 keyup ctrl; sleep 0.3
[ "$(sel)" = 4 ] || fail "Ctrl band made $(sel), not 4"
# A click on blank space clears it.
xdotool mousemove $((x1 + 4)) $((y1 + h1 + 60)) click 1; sleep 0.3
[ "$(sel)" = 0 ] || fail "a click left $(sel) selected"
echo "band ok"

# A folder held under a drag opens; the drop lands inside it.
set -- $(item f2.txt); fx=$(($1 + $3 / 2)); fy=$(($2 + $4 / 3))
set -- $(item deep); dx=$(($1 + $3 / 2)); dy=$(($2 + $4 / 3))
xdotool mousemove "$fx" "$fy" mousedown 1
for t in 1 2 3 4 5 6; do xdotool mousemove $((fx + (dx - fx) * t / 6)) "$fy"; sleep 0.05; done
xdotool mousemove "$dx" "$dy"; sleep 1.4
grep '^state ' "$SB/band.err" | tail -1 | grep -q "dir $SB/c/deep" || fail "the held folder did not open"
xdotool mousemove $((dx + 30)) $((dy + 200)); sleep 0.3; xdotool mouseup 1; sleep 1
[ -f "$SB/c/deep/f2.txt" ] && [ ! -e "$SB/c/f2.txt" ] || fail "drop after opening the folder"
echo "spring folder ok"

# Many files in the list view: a band from beside a name, held below the
# window, scrolls on and takes in rows that were out of sight.
mkdir -p "$SB/many"
i=0; while [ $i -lt 200 ]; do : > "$SB/many/file$(printf %03d $i)"; i=$((i + 1)); done
NIRUFM_PLACES=1 "$BIN" "$SB/many" 2>"$SB/many.err" &
M=$!
PIDS="$PIDS $M"
sleep 1.2
WM=$(xdotool search --pid "$M" | head -1)
xdotool windowmove "$WM" 0 0; xdotool windowraise "$WM"; sleep 0.4
xdotool key ctrl+4; sleep 0.5
grep "^item file000 " "$SB/many.err" | tail -1 > "$SB/row"
read -r _ _ rx ry rw rh < "$SB/row"
[ -n "$rx" ] || fail "list rows not found"
bx=$((rx + rw * 2 / 5)); by=$((ry + rh / 2))
xdotool mousemove "$bx" "$by" mousedown 1
geo=$(xdotool getwindowgeometry "$WM" | sed -n 's/.*Geometry: \([0-9]*\)x\([0-9]*\).*/\2/p')
for y in $((by + 20)) $((by + 60)) $((geo - 20)) $((geo + 10)); do xdotool mousemove "$bx" "$y"; sleep 0.05; done
sleep 1.5
n=$(grep '^state ' "$SB/many.err" | tail -1 | awk '{print $2}')
xdotool mouseup 1; sleep 0.3
[ "$n" -gt 60 ] || fail "edge scrolling took in only $n rows"
echo "band with scrolling ok ($n rows)"

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

# Text fields: the path bar completes with Tab and edits like any field;
# a click places the cursor; the search field takes Home and typing.
mkdir -p "$SB/t/alpha/inner" "$SB/t/beta"
echo new > "$SB/t/a.txt"; echo old > "$SB/t/beta/a.txt"
NIRUFM_PLACES=1 "$BIN" "$SB/t" 2>"$SB/t.err" &
T=$!
PIDS="$PIDS $T"
sleep 1.2
WT=$(xdotool search --pid "$T" | head -1)
xdotool windowmove "$WT" 0 0; xdotool windowfocus --sync "$WT"; sleep 0.3
field() { grep "^field $1 " "$SB/t.err" | tail -1; }
state() { grep '^state ' "$SB/t.err" | tail -1; }
k ctrl+l End
xdotool type --delay 40 "/al"
k Tab
xdotool type --delay 40 "inn"
k Tab Return
state | grep -q "dir $SB/t/alpha/inner\$" || fail "path bar: $(state)"
k ctrl+l
set -- $(field Path); fx=$3; fy=$4
xdotool mousemove $((fx + 30)) $((fy + 8)) click 1; sleep 0.4
c=$(field Path | sed 's/.* cursor \([0-9]*\) anchor \([0-9]*\) .*/\1 \2/')
set -- $c
[ "$1" = "$2" ] && [ "$1" -gt 0 ] && [ "$1" -lt 10 ] || fail "a click in the path bar: $(field Path)"
k Escape BackSpace BackSpace ctrl+f
xdotool type --delay 40 "nn"
k Home i
field Search | grep -q 'text inn$' || fail "search field: $(field Search)"
sleep 0.5
state | grep -q '^state 0 selected 0 ' && fail "search found nothing"
k Escape
echo "text fields ok"

# A name that exists: asked, and Replace, Keep both, Skip (a click) and
# Esc do what they say; the replaced file comes back with Ctrl+Z.
asked() { grep -c "^ask a.txt " "$SB/t.err" || true; }
k End ctrl+c ctrl+l
xdotool type --delay 30 "$SB/t/beta"
k Return
n=$(asked)
k ctrl+v
[ "$(asked)" -gt "$n" ] || fail "no question for a name that exists"
k r
sleep 0.5
[ "$(cat "$SB/t/beta/a.txt")" = new ] || fail "replace"
ls "$XDG_DATA_HOME/Trash/files" | grep -q '^a.txt' || fail "the replaced file is not in the trash"
k ctrl+z
sleep 0.5
[ "$(cat "$SB/t/beta/a.txt")" = old ] || fail "undo of a replace"
k ctrl+v k
sleep 0.5
[ "$(cat "$SB/t/beta/a (2).txt")" = new ] || fail "keep both"
k ctrl+v
set -- $(grep '^answer Skip ' "$SB/t.err" | tail -1)
xdotool mousemove "$3" "$4" click 1; sleep 0.5
[ ! -e "$SB/t/beta/a (3).txt" ] && [ "$(cat "$SB/t/beta/a.txt")" = old ] || fail "skip"
k ctrl+v Escape
sleep 0.5
[ "$(ls "$SB/t/beta" | wc -l)" = 2 ] || fail "Esc in the question: $(ls "$SB/t/beta")"
echo "name conflicts ok"

# New items: an empty file (Ctrl+Alt+N) and one from ~/Templates (the menu
# on blank space), each straight into renaming with its stem selected.
mkdir -p "$HOME/Templates"; echo Dear > "$HOME/Templates/Letter.txt"
k ctrl+alt+n
xdotool type --delay 40 "todo"
k Return
[ -f "$SB/t/beta/todo" ] || fail "new empty file: $(ls "$SB/t/beta")"
xdotool mousemove 700 600 click 3; sleep 0.4
k Down Down Down Down Return
field Rename | grep -q 'cursor 6 anchor 0 text Letter.txt$' || fail "template rename: $(field Rename)"
xdotool type --delay 40 "Memo"
k Return
[ "$(cat "$SB/t/beta/Memo.txt")" = Dear ] || fail "new from a template: $(ls "$SB/t/beta")"
k ctrl+z
sleep 0.5
[ -f "$SB/t/beta/Letter.txt" ] && [ ! -e "$SB/t/beta/Memo.txt" ] || fail "undo of the name"
k ctrl+z
sleep 0.5
[ ! -e "$SB/t/beta/Letter.txt" ] || fail "undo of a new file"
echo "new items ok"

# Redo: Ctrl+Shift+Z and Ctrl+Y do again what Ctrl+Z took back; a new
# action in between clears what could be redone.
k ctrl+shift+z
sleep 0.5
[ -f "$SB/t/beta/Letter.txt" ] || fail "redo of a new file"
k ctrl+y
sleep 0.5
[ -f "$SB/t/beta/Memo.txt" ] && [ ! -e "$SB/t/beta/Letter.txt" ] || fail "redo of a name"
k ctrl+z
sleep 0.5
[ -f "$SB/t/beta/Letter.txt" ] || fail "undo after redo"
k ctrl+alt+n Escape ctrl+y
sleep 0.5
[ -f "$SB/t/beta/Letter.txt" ] && [ ! -e "$SB/t/beta/Memo.txt" ] || fail "redo after a new action"
echo "redo ok"

# Open with: the list of programs; one opens the file and, with "always"
# ticked, becomes the default in mimeapps.list (the rest of it kept).
mkdir -p "$XDG_DATA_HOME/applications"
printf "[Desktop Entry]\nType=Application\nName=Zz Test Editor\nExec=cp %%f $SB/opened\nMimeType=text/plain;\n" > "$XDG_DATA_HOME/applications/zz-test.desktop"
printf '# keep me\n[Added Associations]\nimage/png=x.desktop;\n' > "$XDG_CONFIG_HOME/mimeapps.list"
echo x > "$SB/t/beta/zz.txt"
sleep 0.5
k End Menu
set -- $(grep '^menuitem Open with… ' "$SB/t.err" | tail -1)
[ -n "${4:-}" ] || fail "no Open with… in the menu"
xdotool mousemove "$4" "$5" click 1; sleep 0.5
set -- $(grep '^program zz-test.desktop ' "$SB/t.err" | tail -1)
[ -n "${3:-}" ] || fail "the program is not in the list"
xdotool mousemove "$3" "$4" click 1; sleep 0.3
k space Return
sleep 0.5
[ "$(cat "$SB/opened" 2>/dev/null)" = x ] || fail "Open with did not open"
grep -q '^text/plain=zz-test.desktop;$' "$XDG_CONFIG_HOME/mimeapps.list" || fail "no default: $(cat "$XDG_CONFIG_HOME/mimeapps.list")"
grep -q '^# keep me$' "$XDG_CONFIG_HOME/mimeapps.list" && grep -q '^image/png=x.desktop;$' "$XDG_CONFIG_HOME/mimeapps.list" || fail "mimeapps.list lost lines"
echo "open with ok"

# Permissions in Properties: X makes a file executable, a click flips one
# box; a file of another owner says why it cannot change.
echo 'echo hi' > "$SB/t/beta/run.sh"; chmod 644 "$SB/t/beta/run.sh"
sleep 0.5
xdotool type --delay 40 "run"
k alt+Return x
[ "$(stat -c %a "$SB/t/beta/run.sh")" = 755 ] || fail "executable: $(stat -c %a "$SB/t/beta/run.sh")"
set -- $(grep '^perm 8 ' "$SB/t.err" | tail -1)
xdotool mousemove "$3" "$4" click 1; sleep 0.4
[ "$(stat -c %a "$SB/t/beta/run.sh")" = 754 ] || fail "a click on others run: $(stat -c %a "$SB/t/beta/run.sh")"
k Escape
if [ "$(id -u)" != 0 ] && [ "$(stat -c %u /etc/hostname 2>/dev/null)" = 0 ]; then
  k ctrl+l ctrl+a
  xdotool type --delay 30 "/etc"
  k Return
  xdotool type --delay 40 "hostname"
  k alt+Return space
  grep '^propsnote ' "$SB/t.err" | tail -1 | grep -q 'Only the owner' || fail "no plain error: $(grep '^propsnote' "$SB/t.err" | tail -1)"
  k Escape
fi
echo "permissions ok"
# Type to filter: letters keep the names that hold them (case and accents
# do not matter), Backspace takes one back, Esc shows all, "/" types a path.
mkdir -p "$SB/f/sub" "$SB/fake"
for n in Älg.txt alpha.txt beta.txt café.md; do echo "$n" > "$SB/f/$n"; done
# Stand-ins for the programs "Send to" runs: they write down how they were called.
for tool in niruprodrive xdg-email; do
  printf '#!/bin/sh\necho "$@" >> "%s/%s.log"\n' "$SB" "$tool" > "$SB/fake/$tool"
done
printf '#!/bin/sh\nif [ "$1" = devices ]; then echo %s; exit 0; fi\necho "$@" >> "%s/nirushare.log"\n' \
  "'[{\"host\": \"laptop\", \"online\": true}, {\"host\": \"desk\", \"online\": true}]'" "$SB" > "$SB/fake/nirushare"
chmod +x "$SB/fake/"*
PATH="$SB/fake:$PATH" NIRUFM_PLACES=1 "$BIN" "$SB/f" 2>"$SB/f.err" &
F=$!
PIDS="$PIDS $F"
sleep 1.2
WF=$(xdotool search --pid "$F" | head -1)
xdotool windowmove "$WF" 0 0; xdotool windowfocus --sync "$WF"; sleep 0.3
fstate() { grep '^state ' "$SB/f.err" | tail -1; }
shown() { fstate | awk '{print $4}'; }
xdotool type --delay 60 "alg"; sleep 0.4
[ "$(shown)" = 1 ] || fail "filter alg: $(fstate)"
k BackSpace
[ "$(shown)" = 2 ] || fail "Backspace in the filter: $(fstate)"
k Escape
[ "$(shown)" = 5 ] || fail "Esc did not clear the filter: $(fstate)"
xdotool type --delay 60 "CAFE"; sleep 0.4
[ "$(shown)" = 1 ] && [ "$(fstate | awk '{print $2}')" = 1 ] || fail "filter CAFE: $(fstate)"
k Escape slash
grep '^field Path ' "$SB/f.err" | tail -1 | grep -q 'text /$' || fail "/ does not type a path"
k Escape BackSpace
fstate | grep -q "dir $SB\$" || fail "Backspace with no filter: $(fstate)"
k alt+Left
fstate | grep -q "dir $SB/f\$" || fail "back: $(fstate)"
echo "type to filter ok"

# Folder size on demand (Alt+S) and the SHA-256 in Properties, copied.
want=$(find "$SB/f" -type f -exec stat -c %s {} + | awk '{s += $1} END {print s}')
k alt+s
sleep 0.5
grep '^propssize ' "$SB/f.err" | tail -1 | grep -q "^propssize $want files 4 folders 2 done true stopped false" || fail "folder size: $(grep '^propssize' "$SB/f.err" | tail -1), want $want"
k Escape
n1=$(grep -c '^propssize ' "$SB/f.err")
xdotool type --delay 60 "beta"; sleep 0.3
k alt+Return
[ "$(grep -c '^propssize ' "$SB/f.err")" = "$n1" ] || fail "a size was counted without asking"
k s
sleep 0.6
sum=$(sha256sum "$SB/f/beta.txt" | cut -d' ' -f1)
grep '^propssha ' "$SB/f.err" | tail -1 | grep -q "^propssha $sum\$" || fail "checksum: $(grep '^propssha' "$SB/f.err" | tail -1)"
k ctrl+c Escape Escape ctrl+l ctrl+v
grep '^field Path ' "$SB/f.err" | tail -1 | grep -q "text $sum\$" || fail "the checksum was not copied: $(grep '^field Path' "$SB/f.err" | tail -1)"
k Escape
echo "folder size and checksum ok"

# Send to: Proton Drive and mail only when their programs are there;
# NIRUSHARE to one device or all of them.
xdotool type --delay 60 "alpha"; sleep 0.3
menu_click() {
  k Menu
  set -- $(grep "^menuitem $1 [0-9]* [0-9]*\$" "$SB/f.err" | tail -1 | awk '{print $(NF-1), $NF}')
  [ -n "${2:-}" ] || fail "no menu entry: $(grep '^menuitem' "$SB/f.err" | tail -3)"
  xdotool mousemove "$1" "$2" click 1; sleep 0.6
}
menu_click "Send to Proton Drive"
grep -qx "upload $SB/f/alpha.txt" "$SB/niruprodrive.log" || fail "Proton Drive: $(cat "$SB/niruprodrive.log")"
menu_click "Copy Proton Drive link"
grep -qx "link $SB/f/alpha.txt" "$SB/niruprodrive.log" || fail "Proton Drive link: $(cat "$SB/niruprodrive.log")"
menu_click "Send by email"
grep -qx -- "--attach $SB/f/alpha.txt" "$SB/xdg-email.log" || fail "mail: $(cat "$SB/xdg-email.log")"
menu_click "Send to all my devices"
grep -qx -- "send --all --file $SB/f/alpha.txt" "$SB/nirushare.log" || fail "all devices: $(cat "$SB/nirushare.log")"
menu_click "Send to laptop"
grep -qx -- "send --to laptop --file $SB/f/alpha.txt" "$SB/nirushare.log" || fail "one device: $(cat "$SB/nirushare.log")"
k Escape
echo "send to ok"

# Batch rename: a pattern with a padded number, a regex with a group and
# a change of case, each one Ctrl+Z; a name that exists is refused.
mkdir -p "$SB/r"
for i in 1 2 3; do echo "$i" > "$SB/r/IMG_$i.jpg"; done
echo t > "$SB/r/Trip 01.jpg"
k ctrl+l ctrl+a
xdotool type --delay 30 "$SB/r"
k Return
k Home shift+Right shift+Right F2 Tab
xdotool type --delay 40 "Holiday {n:03}"
k Return
[ -f "$SB/r/Holiday 001.jpg" ] && [ -f "$SB/r/Holiday 003.jpg" ] && [ ! -e "$SB/r/IMG_2.jpg" ] || fail "pattern rename: $(ls "$SB/r")"
k ctrl+z
[ -f "$SB/r/IMG_1.jpg" ] && [ -f "$SB/r/IMG_3.jpg" ] && [ ! -e "$SB/r/Holiday 001.jpg" ] || fail "undo of a pattern rename: $(ls "$SB/r")"
k Home shift+Right shift+Right F2 alt+r
xdotool type --delay 40 'IMG_(\d)'
k Tab
xdotool type --delay 40 'Pic-$1'
k alt+c alt+c Return
[ -f "$SB/r/PIC-1.jpg" ] && [ -f "$SB/r/PIC-3.jpg" ] || fail "regex and case: $(ls "$SB/r")"
k ctrl+z
[ -f "$SB/r/IMG_1.jpg" ] && [ ! -e "$SB/r/PIC-1.jpg" ] || fail "undo of a regex rename: $(ls "$SB/r")"
k Home shift+Right F2 Tab
xdotool type --delay 40 "Trip {n}"
k Return
grep -q '^batch IMG_1.jpg -> Trip 01.jpg problem$' "$SB/f.err" || fail "the name that exists is not flagged"
[ -f "$SB/r/IMG_1.jpg" ] && [ "$(cat "$SB/r/Trip 01.jpg")" = t ] || fail "a name that exists was replaced: $(ls "$SB/r")"
k Escape
echo "batch rename ok"

# Recent: the files of recently-used.xbel that are still there.
printf '<?xml version="1.0"?>\n<xbel version="1.0">\n<bookmark href="file://%s/f/beta.txt" modified="2026-01-02T00:00:00Z"/>\n<bookmark href="file://%s/gone.txt" modified="2026-01-03T00:00:00Z"/>\n</xbel>\n' "$SB" "$SB" > "$XDG_DATA_HOME/recently-used.xbel"
set -- $(grep '^place Recent ' "$SB/f.err" | tail -1)
xdotool mousemove "$3" "$4" click 1; sleep 0.6
[ "$(shown)" = 1 ] && grep -q '^item beta.txt ' "$SB/f.err" || fail "Recent: $(fstate)"
echo "recent ok"

# Compare two panes (Shift+F3): what is only on one side and the newer
# copy are selected; F6 copies them across.
mkdir -p "$SB/L" "$SB/R"
echo same > "$SB/L/a.txt"; cp -p "$SB/L/a.txt" "$SB/R/a.txt"
echo l > "$SB/L/only-l.txt"; echo r > "$SB/R/only-r.txt"
echo new > "$SB/L/new.txt"; echo old > "$SB/R/new.txt"; touch -d 2020-01-01 "$SB/R/new.txt"
k ctrl+l ctrl+a
xdotool type --delay 30 "$SB/L"
k Return F3 Tab ctrl+l ctrl+a
xdotool type --delay 30 "$SB/R"
k Return shift+F3
grep '^compare ' "$SB/f.err" | tail -1 | grep -q '^compare 2 2$' || fail "compare: $(grep '^compare' "$SB/f.err" | tail -1)"
fstate | grep -q "^state 1 selected 3 dir $SB/R\$" || fail "right pane selection: $(fstate)"
k Tab
fstate | grep -q "^state 2 selected 3 dir $SB/L\$" || fail "left pane selection: $(fstate)"
k F6
sleep 0.6
k r
sleep 0.6
[ "$(cat "$SB/R/new.txt")" = new ] && [ -f "$SB/R/only-l.txt" ] || fail "copy across: $(ls "$SB/R")"
grep '^compare ' "$SB/f.err" | tail -1 | grep -q '^compare 0 1$' || fail "compare after the copy: $(grep '^compare' "$SB/f.err" | tail -1)"
k shift+F3
echo "compare ok"
echo "PASS"
