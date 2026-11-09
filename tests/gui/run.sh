#!/usr/bin/env bash
# GUI smoke tests: drive the real app under Xvfb (X11) and headless sway
# (Wayland) with simulated input, and check real outcomes (files, clipboard,
# window titles, exit codes). On failure, a screenshot and the app's log are
# kept in $OUT for the CI artifact.
#
# Needs: Xvfb xdotool xclip (X11); sway wtype wl-clipboard grim (Wayland);
# and the release build plus tests/gui/testkit built in release mode.
#
# Usage: tests/gui/run.sh [test-name-substring ...]

set -u
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
APP=${APP:-$ROOT/target/release/plainpad}
KIT=${KIT:-$ROOT/tests/gui/testkit/target/release/plainpad-testkit}
OUT=${OUT:-$ROOT/target/gui-test-output}
WORK=$(mktemp -d)
mkdir -p "$OUT"
FILTER=("$@")
PASSED=0
FAILED=()

# ---------------------------------------------------------------- helpers

log() { echo "    $*"; }

# wait_until SECONDS CMD...: poll CMD every 0.1 s until it succeeds.
wait_until() {
    local deadline=$((SECONDS + $1)); shift
    until "$@" >/dev/null; do
        [ $SECONDS -ge $deadline ] && return 1
        sleep 0.1
    done
}

file_is() { [ "$(cat "$1")" = "$2" ]; }
file_has() { grep -qF -- "$2" "$1"; }

# A fresh app instance with its own settings/recovery folder; sets APP_PID and CFG.
start_app() {
    CFG=$(mktemp -d -p "$WORK")
    XDG_CONFIG_HOME=$CFG "$APP" "$@" >"$WORK/app.log" 2>&1 &
    APP_PID=$!
}

stop_apps() {
    pkill -x plainpad 2>/dev/null
    wait_until 5 sh -c '! pgrep -x plainpad >/dev/null' || pkill -9 -x plainpad 2>/dev/null
}

screenshot() {
    if [ -n "${WAYLAND_DISPLAY:-}" ]; then grim "$1" 2>/dev/null
    else import -window root "$1" 2>/dev/null || xwd -root -silent >"$1.xwd" 2>/dev/null; fi
}

run_test() {
    local name=$1
    if [ ${#FILTER[@]} -gt 0 ]; then
        local hit=0
        for f in "${FILTER[@]}"; do [[ $name == *$f* ]] && hit=1; done
        [ $hit = 1 ] || return 0
    fi
    echo "--- $name"
    local dir
    dir=$(mktemp -d -p "$WORK")
    if (cd "$dir" && set -e && "$name"); then
        echo "    PASS"
        PASSED=$((PASSED + 1))
    else
        echo "    FAIL"
        FAILED+=("$name")
        screenshot "$OUT/$name.png"
        cp "$WORK/app.log" "$OUT/$name.log" 2>/dev/null
    fi
    stop_apps
}

# ---------------------------------------------------------------- X11

# Prints the first window whose title matches; fails (so wait_until keeps polling) if none does.
x_win() { local w; w=$(xdotool search --name "$1" 2>/dev/null | head -1); [ -n "$w" ] && echo "$w"; }
x_focus() { local w; w=$(x_win "$1") || return 1; xdotool windowsize "$w" 1000 640; xdotool windowmove "$w" 0 0; xdotool windowfocus "$w"; sleep 0.3; }
x_count() { xdotool search --name "Plainpad" 2>/dev/null | wc -l; }

x11_type_and_save() {
    printf 'hello\n' > a.txt
    start_app a.txt
    wait_until 10 x_win "a.txt - Plainpad"
    x_focus "a.txt"
    xdotool key ctrl+End; xdotool type "world"; xdotool key ctrl+s
    wait_until 5 file_is a.txt $'hello\nworld'
    # The title loses its modified marker once saved.
    wait_until 5 x_win "^a.txt - Plainpad"
}

x11_clipboard() {
    printf 'alpha' > c.txt
    start_app c.txt
    wait_until 10 x_win "c.txt"
    x_focus "c.txt"
    xdotool key ctrl+a ctrl+c
    wait_until 5 sh -c '[ "$(timeout 2 xclip -o -selection clipboard)" = alpha ]'
    echo -n "+pasted" | xclip -selection clipboard
    sleep 0.2
    xdotool key End ctrl+v
    sleep 0.5
    xdotool key ctrl+s
    wait_until 5 file_is c.txt "alpha+pasted"
}

x11_tabs_close_prompt() {
    printf 'one\n' > t1.txt; printf 'two\n' > t2.txt
    start_app t1.txt t2.txt
    wait_until 10 x_win "t2.txt"
    x_focus "t2.txt"
    xdotool key ctrl+1; xdotool type "X"
    wait_until 5 x_win '\*t1.txt'
    xdotool key ctrl+w; sleep 0.3
    xdotool key Tab Return   # Don't Save
    wait_until 5 x_win "^t2.txt - Plainpad"
    file_is t1.txt "one"
}

x11_drag_tab_out() {
    printf '1\n' > d1.txt; printf '2\n' > d2.txt
    start_app d1.txt d2.txt
    wait_until 10 x_win "d2.txt"
    x_focus "d2.txt"
    xdotool mousemove 40 43 mousedown 1; sleep 0.1
    xdotool mousemove 60 150; sleep 0.1; xdotool mousemove 700 450; sleep 0.2; xdotool mouseup 1
    wait_until 5 sh -c '[ "$(xdotool search --name Plainpad | wc -l)" = 2 ]'
}

x11_crash_recovery() {
    printf 'saved\n' > r.txt
    start_app r.txt
    wait_until 10 x_win "r.txt"
    x_focus "r.txt"
    xdotool key ctrl+End; xdotool type "unsaved"
    wait_until 6 sh -c "find '$CFG' -name '*.snap' | grep -q ."
    kill -9 "$APP_PID"; wait "$APP_PID" 2>/dev/null; sleep 0.3
    file_is r.txt "saved"
    # Restart with the same settings folder: the edit comes back.
    XDG_CONFIG_HOME=$CFG "$APP" >"$WORK/app.log" 2>&1 &
    wait_until 10 x_win '\*r.txt'
    x_focus "r.txt"
    xdotool key ctrl+s
    wait_until 5 file_is r.txt $'saved\nunsaved'
}

x11_reload_changed_file() {
    printf 'v1\n' > e.txt; printf 'other\n' > o.txt
    start_app e.txt
    wait_until 10 x_win "e.txt"
    XDG_CONFIG_HOME=$(mktemp -d -p "$WORK") "$APP" o.txt >/dev/null 2>&1 &
    wait_until 10 x_win "o.txt"
    x_focus "e.txt"; xdotool windowfocus "$(x_win o.txt)"; sleep 0.3
    printf 'v2 from outside\n' > e.txt
    x_focus "e.txt"   # regaining focus reloads the clean tab
    sleep 0.5
    xdotool key ctrl+a ctrl+c
    wait_until 5 sh -c '[ "$(timeout 2 xclip -o -selection clipboard)" = "v2 from outside" ]'
}

# Types one character at the start of the document, retrying until it takes:
# big files load in the background and stay read-only until then.
x_mark_when_loaded() {
    local name=$1 deadline=$((SECONDS + 15))
    while [ $SECONDS -lt $deadline ]; do
        xdotool key ctrl+Home; xdotool type "#"
        wait_until 1 x_win "\\*$name" && return 0
    done
    return 1
}

x11_long_line() {
    python3 -c "print('abc def; ' * 600000, end='')" > long.txt
    local t0=$SECONDS
    start_app long.txt
    wait_until 5 x_win "long.txt"   # used to take 5+ s
    x_focus "long.txt"
    x_mark_when_loaded long.txt
    xdotool key ctrl+Home; xdotool type "START"; xdotool key ctrl+s
    wait_until 10 sh -c 'head -c 6 long.txt | grep -q "START#"'
    [ $((SECONDS - t0)) -lt 15 ]
}

# A 60 MB file opens, saves and is searched in the background: the window
# appears at once, keeps responding, and the results are right.
x11_big_file() {
    python3 -c "
import sys
w = sys.stdout.write
for i in range(1_200_000): w(f'line {i:07d} brown fox\\n')
w('needle at the end\\n')" > big.txt
    local size; size=$(stat -c %s big.txt)
    start_app big.txt
    wait_until 2 x_win "big.txt"
    x_focus "big.txt"
    x_mark_when_loaded big.txt
    # Save in the background; the file is complete and the title clean afterwards.
    xdotool key ctrl+s
    wait_until 10 x_win "^big.txt - Plainpad"
    [ "$(stat -c %s big.txt)" = $((size + 1)) ]
    [ "$(head -c 13 big.txt)" = "#line 0000000" ]
    [ "$(tail -n 1 big.txt)" = "needle at the end" ]
    # Find the last line from the top (a background search), then type over it.
    xdotool key ctrl+f; sleep 0.2; xdotool type "NEEDLE AT"; xdotool key Return Escape; sleep 1
    xdotool type "found"; xdotool key ctrl+s
    wait_until 10 sh -c '[ "$(tail -n 1 big.txt)" = "found the end" ]'
}

# ---------------------------------------------------------------- Wayland

PTR_FIFO=$WORK/pointer.fifo

p() { timeout 2 sh -c "echo '$*' > '$PTR_FIFO'"; sleep 0.08; }
# wtype drops the first key after it creates its virtual keyboard: lead with Escape.
k() { timeout 5 wtype -k Escape "$@"; }
w_count() { swaymsg -t get_tree | grep -c '"name": "[^"]*Plainpad"'; }
w_title_has() { swaymsg -t get_tree | grep -qF "\"name\": \"$1"; }

wl_clipboard() {
    printf 'alpha' > c.txt
    start_app c.txt
    wait_until 10 w_title_has "c.txt"
    sleep 0.5
    k -M ctrl a -m ctrl -M ctrl c -m ctrl
    wait_until 5 sh -c '[ "$(timeout 2 wl-paste -n)" = alpha ]'
    echo -n "+in" | timeout 2 wl-copy; sleep 0.3
    k -k Right -M ctrl v -m ctrl; sleep 0.5
    k -M ctrl s -m ctrl
    wait_until 5 file_is c.txt "alpha+in"
}

wl_paste_does_not_freeze() {
    printf '' > h.txt
    start_app h.txt
    wait_until 10 w_title_has "h.txt"
    sleep 0.5
    "$KIT" hang-clipboard 2>/dev/null &
    local hang=$!
    sleep 0.5
    k -M ctrl v -m ctrl
    # While the paste is stuck, typing and saving still work.
    k typed -M ctrl s -m ctrl
    wait_until 3 file_is h.txt "typed"
    kill $hang
}

wl_drag_tab_between_windows() {
    printf '1\n' > w1.txt; printf '2\n' > w2.txt; printf '3\n' > w3.txt
    start_app w1.txt w2.txt w3.txt
    wait_until 10 w_title_has "w3.txt"
    sleep 0.5
    # Drag the second tab out: a new window.
    p move 130 68; p down; for y in 90 150 300 400 450; do p move 400 $y; done; p up
    wait_until 5 sh -c '[ "$(swaymsg -t get_tree | grep -c "Plainpad")" -ge 2 ]'
    [ "$(w_count)" = 2 ]
    # Drag a tab from the left window onto the right window's tab bar.
    p move 40 68; p down; for x in 60 120 300 500 700 760; do p move $x 140; done; p move 760 70; p move 765 68; p up
    sleep 1
    [ "$(w_count)" = 2 ]
}

wl_clean_exit() {
    printf 'x\n' > q.txt
    start_app q.txt
    wait_until 10 w_title_has "q.txt"
    sleep 0.5
    k -M ctrl q -m ctrl
    wait_until 5 sh -c "! kill -0 $APP_PID 2>/dev/null"
    wait "$APP_PID"
}

# ---------------------------------------------------------------- main

[ -x "$APP" ] || { echo "build the app first: cargo build --release"; exit 2; }

echo "=== X11 (Xvfb)"
Xvfb :91 -screen 0 1100x700x24 >/dev/null 2>&1 &
XVFB=$!
export DISPLAY=:91
unset WAYLAND_DISPLAY
wait_until 5 xdpyinfo >/dev/null 2>&1 || sleep 1
for t in x11_type_and_save x11_clipboard x11_tabs_close_prompt x11_drag_tab_out x11_crash_recovery \
         x11_reload_changed_file x11_long_line x11_big_file; do
    run_test $t
done
kill $XVFB 2>/dev/null
unset DISPLAY

if command -v sway >/dev/null && [ -x "$KIT" ]; then
    echo "=== Wayland (headless sway)"
    export XDG_RUNTIME_DIR=$WORK/xdg
    mkdir -p "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
    printf 'output HEADLESS-1 resolution 1280x800\n' > "$WORK/sway.conf"
    WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
        sway -c "$WORK/sway.conf" >"$WORK/sway.log" 2>&1 &
    SWAY=$!
    wait_until 10 sh -c "ls '$XDG_RUNTIME_DIR' | grep -q '^wayland-[0-9]*$'"
    export WAYLAND_DISPLAY
    WAYLAND_DISPLAY=$(ls "$XDG_RUNTIME_DIR" | grep '^wayland-[0-9]*$' | head -1)
    wait_until 5 sh -c "ls '$XDG_RUNTIME_DIR' | grep -q 'sway-ipc'"
    export SWAYSOCK
    SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock | head -1)
    mkfifo "$PTR_FIFO"
    (tail -f "$PTR_FIFO" | "$KIT" pointer >/dev/null 2>&1 &)
    sleep 0.5
    for t in wl_clipboard wl_paste_does_not_freeze wl_drag_tab_between_windows wl_clean_exit; do
        run_test $t
    done
    pkill -f "plainpad-testkit pointer" 2>/dev/null
    pkill -f "tail -f $PTR_FIFO" 2>/dev/null
    kill $SWAY 2>/dev/null
else
    echo "=== Wayland tests skipped (needs sway and the testkit)"
fi

rm -rf "$WORK"
echo
echo "passed: $PASSED, failed: ${#FAILED[@]} ${FAILED[*]:-}"
[ ${#FAILED[@]} -eq 0 ]
