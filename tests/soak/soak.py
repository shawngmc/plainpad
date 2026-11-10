#!/usr/bin/env python3
"""Long-session soak test: drives the release build with random (seeded) user
actions for a long time and checks that memory returns to its starting level.

Every few minutes the app is brought back to the same state (checkpoint):
extra windows are emptied back into the main one, untitled tabs are discarded,
file tabs are saved and their files rewritten with the original text (so the
tabs reload, which also clears their undo history), zoom, word wrap and window
size are reset, and the clipboard is handed to another program. Then these are
recorded: memory still allocated (from glibc's malloc_stats, through gdb), the
process's own memory (RssAnon), its total memory, its CPU use while idle and
how fast it responds. The baseline is the second checkpoint, after a warm-up
round (the first shows every tab, which can load fallback fonts).

It fails if the app crashes or stops responding, if it uses CPU while idle, if
memory still allocated at the last checkpoint is more than 10% + 1 MB above
the baseline (a leak), or if the process's own memory is more than 50% (at
least 10 MB) above it (memory the allocator keeps; this wanders by several MB
between checkpoints, so the limit is looser).

Needs gdb for the leak check (with passwordless sudo when not root, as on CI
runners); without it, only the process's memory is checked.

Usage: tests/soak/soak.py [--minutes 60] [--round-minutes 5] [--seed N]

Needs Xvfb, xdotool and xclip. Writes target/soak/ (results.json, the action
log, the app's log, and a screenshot on failure) and a Markdown summary to
$GITHUB_STEP_SUMMARY in GitHub Actions.
"""

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = Path(os.environ.get("APP", ROOT / "target/release/plainpad"))
OUT = ROOT / "target/soak"
DISPLAY = os.environ.get("SOAK_DISPLAY", ":94")
TICK_MS = 1000 / os.sysconf("SC_CLK_TCK")
W, H = 1000, 640  # main window size; click positions below assume it

WORDS = ("the quick brown fox jumps over the lazy dog lorem ipsum dolor sit amet "
         "https://example.com/a?b=c www.rust-lang.org").split()
UNICODE = ["é", "naïve", "Ünïcödé", "日本語", "中文字", "مرحبا", "שלום", "😀", "ﬁ", "Ελληνικά", "русский"]


class Failure(Exception):
    pass


def sh(*args, input=None):
    return subprocess.run(list(map(str, args)), capture_output=True, text=True, input=input)


def x(*args):
    sh("xdotool", *map(str, args))


def wait(pred, timeout, step=0.02):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        v = pred()
        if v:
            return v
        time.sleep(step)
    return None


def proc_status(pid):
    out = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        k, _, v = line.partition(":")
        if k in ("VmRSS", "RssAnon", "VmHWM"):
            out[k] = int(v.split()[0])
    return out


def live_kb(pid, log):
    """Bytes the app has allocated and not freed (all arenas, plus large mapped
    blocks), from glibc's malloc_stats, called in the app through gdb. Unlike
    the process's memory, this doesn't depend on fragmentation, so it shows a
    real leak. None if gdb can't attach."""
    gdb = ["gdb", "-p", str(pid), "-batch", "-ex", "call (void) malloc_stats()"]
    if os.geteuid() != 0 and shutil.which("sudo"):
        gdb = ["sudo", "-n", *gdb]  # attaching to a non-child needs privileges on Ubuntu
    try:
        subprocess.run(gdb, capture_output=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    time.sleep(0.3)
    text = log.read_text(errors="replace")
    total = text.rfind("Total (incl. mmap):")
    for line in text[total:].splitlines() if total >= 0 else []:
        if line.startswith("in use bytes"):
            return int(line.split("=")[1]) // 1024
    return None


def cpu_ms(pid):
    f = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) * TICK_MS


def make_files(d):
    files = {
        "notes.txt": "".join(f"line {i}: {' '.join(WORDS[i % 7:i % 7 + 6])}\n" for i in range(300)),
        "mixed.txt": "".join(f"{i} {' '.join(UNICODE)} {' '.join(WORDS[:5])}\n" for i in range(200)),
        "big.txt": "The quick brown fox jumps over the lazy dog, https://example.com/path and more.\n" * 75_000,
        "long.txt": "abc def; ghi, " * 22_000,
    }
    for name, text in files.items():
        (d / name).write_text(text)
    return files


class Soak:
    def __init__(self, args, work):
        self.rng = random.Random(args.seed)
        self.work = work
        self.originals = make_files(work)
        self.files = list(self.originals)
        self.cfg = tempfile.mkdtemp(prefix="soak-cfg-")
        self.log = open(OUT / "actions.log", "w")
        env = dict(os.environ, XDG_CONFIG_HOME=self.cfg, DISPLAY=DISPLAY)
        env.pop("WAYLAND_DISPLAY", None)
        self.applog = open(OUT / "app.log", "w")
        # A second instance, only for moving focus away and back (which makes the
        # main instance check its files on disk). Started first, so its window is
        # known and can be told apart (the app doesn't set _NET_WM_PID), and parked
        # in a corner where it never overlaps.
        self.sink = subprocess.Popen([str(APP)], env=dict(env, XDG_CONFIG_HOME=tempfile.mkdtemp()),
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.sink_win = wait(lambda: self.all_windows()[:1], 15)
        if not self.sink_win:
            raise Failure("the helper window never appeared")
        self.sink_win = self.sink_win[0]
        x("windowsize", self.sink_win, 200, 150)
        x("windowmove", self.sink_win, 1500, 950)
        self.p = subprocess.Popen([str(APP), *(str(work / f) for f in self.files)], env=env,
                                  stdout=self.applog, stderr=subprocess.STDOUT)
        self.main = wait(lambda: self.windows()[:1], 15)
        if not self.main:
            raise Failure("the app's window never appeared")
        self.main = self.main[0]
        self.wrap_flips = 0
        self.actions = 0
        self.new_tabs = 0  # untitled tabs opened since the last checkpoint, at most
        self.place_main()
        # Big files load in the background; give them a moment.
        time.sleep(2)

    # ------------------------------------------------------------ helpers

    @staticmethod
    def all_windows():
        return sorted(sh("xdotool", "search", "--name", "Plainpad").stdout.split(), key=int)

    def windows(self):
        """The tested instance's windows, oldest first."""
        return [w for w in self.all_windows() if w != self.sink_win]

    def alive(self):
        if self.p.poll() is not None:
            raise Failure(f"the app exited (code {self.p.returncode}) after {self.actions} actions")

    def place_main(self):
        x("windowmove", self.main, 0, 0)
        x("windowsize", self.main, W, H)
        x("windowfocus", "--sync", self.main)
        time.sleep(0.15)

    def title(self):
        time.sleep(0.05)
        return sh("xdotool", "getwindowname", self.main).stdout.strip()

    def active_name(self):
        t = self.title()
        return t.removeprefix("*").removesuffix(" - Plainpad"), t.startswith("*")

    def note(self, what):
        self.log.write(f"{time.monotonic():.2f} {what}\n")

    def text(self, n):
        out = []
        for _ in range(n):
            pool = UNICODE if self.rng.random() < 0.15 else WORDS
            out.append(self.rng.choice(pool))
        return " ".join(out)

    def clip(self, text):
        # xclip forks to serve the selection; it exits once another program takes it.
        subprocess.run(["xclip", "-selection", "clipboard"], input=text, text=True, timeout=10)

    def drag(self, x0, y0, x1, y1):
        x("mousemove", x0, y0)
        x("mousedown", 1)
        time.sleep(0.05)
        for k in range(1, 6):
            x("mousemove", x0 + (x1 - x0) * k // 5, y0 + (y1 - y0) * k // 5)
            time.sleep(0.04)
        x("mouseup", 1)
        time.sleep(0.3)

    # ------------------------------------------------------------ actions

    def a_type(self):
        x("type", "--delay", 4, self.text(self.rng.randint(1, 12)) + " ")

    def a_enter(self):
        x("key", "--repeat", self.rng.randint(1, 3), "Return")

    def a_delete(self):
        key = self.rng.choice(["BackSpace", "Delete", "ctrl+BackSpace", "ctrl+Delete"])
        x("key", "--repeat", self.rng.randint(1, 20), "--delay", 2, key)

    def a_move(self):
        key = self.rng.choice(["Left", "Right", "Up", "Down", "Home", "End", "Prior", "Next", "ctrl+Home",
                               "ctrl+End", "ctrl+Left", "ctrl+Right"])
        if self.rng.random() < 0.3:
            key = "shift+" + key
        x("key", "--repeat", self.rng.randint(1, 8), "--delay", 2, key)

    def a_click(self):
        x("mousemove", "--window", self.main, self.rng.randint(20, W - 40), self.rng.randint(70, H - 120))
        x("click", "--repeat", self.rng.choice([1, 1, 1, 2, 3]), "--delay", 30, 1)

    def a_scroll(self):
        x("mousemove", "--window", self.main, 400, 300)
        x("click", "--repeat", self.rng.randint(1, 15), "--delay", 5, self.rng.choice([4, 5]))

    def a_undo(self):
        x("key", "--repeat", self.rng.randint(1, 30), "--delay", 2, self.rng.choice(["ctrl+z", "ctrl+y"]))

    def a_select_all(self):
        x("key", "ctrl+a")
        x("type", "--delay", 4, self.text(3)) if self.rng.random() < 0.5 else x("key", "Delete")

    def a_copy_paste(self):
        x("key", "shift+End", self.rng.choice(["ctrl+c", "ctrl+x"]), "End", "ctrl+v")

    def a_paste_outside(self):
        size = self.rng.choice([100, 10_000, 200_000, 1_000_000, 3_000_000])
        line = self.text(12) + "\n"
        self.clip((line * (size // len(line) + 1))[:size])
        x("key", "ctrl+v")
        time.sleep(0.5)

    def a_new_tab(self):
        if self.new_tabs >= 12:
            return self.a_switch_tab()
        self.new_tabs += 1
        x("key", "ctrl+n")
        x("type", "--delay", 4, self.text(5))

    def a_switch_tab(self):
        x("key", self.rng.choice(["ctrl+Tab", "ctrl+shift+Tab", "ctrl+1", "ctrl+3", "ctrl+9", "ctrl+Next"]))

    def a_close_tab(self):
        name, dirty = self.active_name()
        if name.startswith("Untitled"):
            x("key", "ctrl+w")
            if dirty:
                time.sleep(0.2)
                x("key", "Tab", "Return")  # Don't Save

    def a_save(self):
        name, dirty = self.active_name()
        if dirty and name in self.files:
            x("key", "ctrl+s")
            time.sleep(0.3)

    def a_find(self):
        x("key", "ctrl+f", "ctrl+a")
        x("type", "--delay", 4, self.rng.choice(WORDS + UNICODE))
        x("key", "--repeat", self.rng.randint(1, 5), "--delay", 20, self.rng.choice(["Return", "shift+F3"]))
        x("key", "Escape")

    def a_replace_all(self):
        self.place_main()
        x("key", "ctrl+h", "ctrl+a")
        x("type", "--delay", 4, self.rng.choice(WORDS))
        x("mousemove", "--window", self.main, 275, 597)  # the Replace field
        x("click", 1)
        x("key", "ctrl+a")
        x("type", "--delay", 4, self.rng.choice(WORDS + UNICODE))
        x("mousemove", "--window", self.main, 602, 597)  # Replace all
        x("click", 1)
        time.sleep(0.3)
        x("key", "Escape")

    def a_zoom(self):
        x("key", "--repeat", self.rng.randint(1, 4), "--delay", 20, self.rng.choice(["ctrl+equal", "ctrl+minus"]))

    def a_wrap(self):
        x("key", "alt+z")
        self.wrap_flips += 1

    def a_resize(self):
        x("windowsize", self.main, self.rng.randint(500, 1500), self.rng.randint(300, 950))
        time.sleep(0.2)
        self.place_main()

    def a_context_menu(self):
        x("mousemove", "--window", self.main, self.rng.randint(50, 600), self.rng.randint(80, 400))
        x("click", 3)
        time.sleep(0.1)
        x("key", "Down", "Escape")

    def a_drag_tab(self):
        """Drags the first tab out into a new window, then back."""
        self.place_main()
        before = len(self.windows())
        self.drag(40, 43, 700, 450)
        if wait(lambda: len(self.windows()) > before, 3):
            self.empty_extra_windows()
        self.place_main()

    ACTIONS = [
        (a_type, 30), (a_enter, 6), (a_delete, 10), (a_move, 14), (a_click, 6), (a_scroll, 6), (a_undo, 6),
        (a_select_all, 2), (a_copy_paste, 4), (a_paste_outside, 2), (a_new_tab, 2), (a_switch_tab, 5),
        (a_close_tab, 2), (a_save, 2), (a_find, 3), (a_replace_all, 1), (a_zoom, 2), (a_wrap, 1),
        (a_resize, 1), (a_context_menu, 2), (a_drag_tab, 1),
    ]

    def run_round(self, seconds):
        fns, weights = zip(*self.ACTIONS)
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            fn = self.rng.choices(fns, weights)[0]
            self.note(fn.__name__)
            fn(self)
            self.actions += 1
            time.sleep(self.rng.uniform(0.01, 0.05))
            self.alive()

    # ------------------------------------------------------------ checkpoints

    def empty_extra_windows(self):
        """Drags every tab of every other window back onto the main window's tab bar."""
        for _ in range(30):
            extra = [w for w in self.windows() if w != self.main]
            if not extra:
                return
            w = extra[0]
            out = sh("xdotool", "getwindowgeometry", "--shell", w).stdout
            geo = dict(line.split("=", 1) for line in out.splitlines() if "=" in line)
            wx, wy = int(geo["X"]), int(geo["Y"])
            x("windowfocus", "--sync", w)
            self.drag(wx + 40, wy + 43, 300, 43)
        raise Failure("couldn't move tabs back from an extra window")

    def checkpoint(self):
        """Brings the app back to its starting state and measures it."""
        self.note("checkpoint")
        # Let pastes still in flight land first (a big one takes a moment), and
        # make sure no later one brings big text back.
        self.clip("x")
        time.sleep(2)
        x("key", "Escape", "Escape", "Escape")
        self.empty_extra_windows()
        self.place_main()
        x("key", "Escape", "Escape")
        # Discard untitled tabs; save modified file tabs.
        clean_run = 0
        self.new_tabs = 0
        for _ in range(400):
            name, dirty = self.active_name()
            if name.startswith("Untitled"):
                x("key", "ctrl+w")
                if dirty:
                    time.sleep(0.25)
                    x("key", "Tab", "Return")
                clean_run = 0
                continue
            if dirty:
                x("key", "ctrl+s")
                if not wait(lambda: not self.active_name()[1], 20, 0.1):
                    raise Failure(f"saving {name} never finished")
                clean_run = 0
            clean_run += 1
            if clean_run > len(self.files):
                break
            x("key", "ctrl+Tab")
        else:
            raise Failure("couldn't get back to the starting tabs")
        # Put the original text back on disk; moving focus away and back reloads the (clean) tabs.
        for name, text in self.originals.items():
            (self.work / name).write_text(text)
        x("windowfocus", "--sync", self.sink_win)
        time.sleep(0.3)
        self.place_main()
        time.sleep(3)  # the big file reloads in the background
        if self.wrap_flips % 2:
            x("key", "alt+z")
            self.wrap_flips = 0
        # Reset zoom; changing it also empties the glyph cache.
        x("key", "ctrl+0", "ctrl+equal", "ctrl+0", "ctrl+1", "ctrl+Home")
        self.clip("x")
        # Responsiveness: how long a new tab takes to appear.
        t = time.monotonic()
        x("key", "ctrl+n")
        if not wait(lambda: self.title().startswith("Untitled"), 10, 0.005):
            raise Failure("the app stopped responding")
        probe_ms = (time.monotonic() - t) * 1000
        x("key", "ctrl+w")
        time.sleep(3)
        self.alive()
        cpu0 = cpu_ms(self.p.pid)
        time.sleep(2)
        idle = cpu_ms(self.p.pid) - cpu0
        st = proc_status(self.p.pid)
        self.applog.flush()
        live = live_kb(self.p.pid, OUT / "app.log")
        return {"actions": self.actions, "live_kb": live, "heap_kb": st["RssAnon"], "rss_kb": st["VmRSS"],
                "peak_rss_kb": st["VmHWM"], "idle_cpu_ms": round(idle), "probe_ms": round(probe_ms)}

    def close(self):
        for p in (self.p, self.sink):
            if p.poll() is None:
                p.kill()
                p.wait()
        self.log.close()
        self.applog.close()
        shutil.rmtree(self.cfg, ignore_errors=True)


def report(points, verdicts):
    lines = ["| Minute | Actions | In use | Own memory | Total (RSS) | Peak RSS | Idle CPU (2 s) | New tab |",
             "|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for p in points:
        live = "–" if p["live_kb"] is None else f"{p['live_kb'] / 1024:.1f} MB"
        lines.append(f"| {p['minute']:.0f} | {p['actions']:,} | {live} | {p['heap_kb'] / 1024:.1f} MB | "
                     f"{p['rss_kb'] / 1024:.1f} MB | {p['peak_rss_kb'] / 1024:.1f} MB | {p['idle_cpu_ms']} ms | "
                     f"{p['probe_ms']} ms |")
    return "\n".join(lines) + "\n\n" + "\n".join(f"- {v}" for v in verdicts)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--minutes", type=float, default=60)
    ap.add_argument("--round-minutes", type=float, default=5)
    ap.add_argument("--warmup-minutes", type=float, default=1)
    ap.add_argument("--seed", type=int, default=int(time.time()))
    args = ap.parse_args()
    if not APP.exists():
        sys.exit(f"{APP} not found: cargo build --release first")
    OUT.mkdir(parents=True, exist_ok=True)
    print(f"soak: {args.minutes} min, seed {args.seed}", flush=True)
    xvfb = subprocess.Popen(["Xvfb", DISPLAY, "-screen", "0", "1700x1100x24"], stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    os.environ["DISPLAY"] = DISPLAY
    work = Path(tempfile.mkdtemp(prefix="soak-"))
    points, verdicts, failed, soak = [], [], False, None
    start = time.monotonic()
    try:
        wait(lambda: sh("xdotool", "getmouselocation").returncode == 0, 10, 0.1)
        soak = Soak(args, work)
        soak.run_round(args.warmup_minutes * 60)
        # The first checkpoint shows every tab, which can load fallback fonts for the
        # first time; the baseline is the next one.
        soak.checkpoint()
        end = start + args.minutes * 60
        while True:
            p = soak.checkpoint()
            p["minute"] = (time.monotonic() - start) / 60
            points.append(p)
            print(f"  minute {p['minute']:.1f}: {json.dumps(p)}", flush=True)
            left = end - time.monotonic()
            if left <= 30:
                break
            soak.run_round(min(args.round_minutes * 60, left))
        base, last = points[0], points[-1]
        # Leaks: memory still allocated must come back to where it started.
        if base["live_kb"] is not None and last["live_kb"] is not None:
            limit = base["live_kb"] * 1.1 + 1024
            msg = (f"memory in use {base['live_kb'] / 1024:.1f} MB -> {last['live_kb'] / 1024:.1f} MB "
                   f"(limit {limit / 1024:.1f} MB)")
            failed |= last["live_kb"] > limit
            verdicts.append(("FAIL: " if last["live_kb"] > limit else "ok: ") + msg)
        else:
            verdicts.append("skipped: memory in use (gdb couldn't attach)")
        # Bloat: the process's own memory also includes what the allocator holds
        # on to (fragmentation), which wanders by several MB from checkpoint to
        # checkpoint, so it gets a looser limit.
        limit = base["heap_kb"] + max(base["heap_kb"] * 0.5, 10240)
        msg = (f"process's own memory {base['heap_kb'] / 1024:.1f} MB -> {last['heap_kb'] / 1024:.1f} MB "
               f"(limit {limit / 1024:.1f} MB)")
        failed |= last["heap_kb"] > limit
        verdicts.append(("FAIL: " if last["heap_kb"] > limit else "ok: ") + msg)
        busy = [p for p in points if p["idle_cpu_ms"] > 30]
        if busy:
            failed = True
            verdicts.append(f"FAIL: CPU used while idle at {len(busy)} checkpoint(s)")
        else:
            verdicts.append("ok: no CPU used while idle")
        verdicts.append(f"ok: {last['actions']:,} actions in {last['minute']:.0f} minutes without a crash")
    except Failure as e:
        failed = True
        verdicts.append(f"FAIL: {e}")
        sh("import", "-window", "root", str(OUT / "failure.png"))
    finally:
        if soak:
            soak.close()
        xvfb.kill()
        shutil.rmtree(work, ignore_errors=True)
    verdicts.append(f"seed {args.seed} (rerun with --seed {args.seed}; actions in target/soak/actions.log)")
    (OUT / "results.json").write_text(json.dumps({"seed": args.seed, "checkpoints": points,
                                                  "verdicts": verdicts}, indent=2) + "\n")
    table = report(points, verdicts)
    print(table)
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as f:
            f.write("## Soak test\n\n" + table + "\n")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
