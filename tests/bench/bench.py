#!/usr/bin/env python3
"""Benchmarks the release build in a real X11 session (Xvfb) and compares the
results with tests/bench/baseline.json, failing on regressions.

Memory and binary size are steady from run to run, so they fail at more than
20% worse than the baseline. Timings on shared CI machines vary a lot, so
they only fail at 3x the baseline and warn at 1.5x.

Usage:
  tests/bench/bench.py            measure and compare
  tests/bench/bench.py --update   measure and write the results as the new baseline

Needs Xvfb and xdotool. Writes target/bench/results.json, and a Markdown table
to $GITHUB_STEP_SUMMARY when run in GitHub Actions.
"""

import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = Path(os.environ.get("APP", ROOT / "target/release/plainpad"))
BASELINE = ROOT / "tests/bench/baseline.json"
OUT = ROOT / "target/bench"
DISPLAY = ":93"
TICK_MS = 1000 / os.sysconf("SC_CLK_TCK")

# name: (description, unit, kind, slack). kind sets the ratios; a change
# within `slack` (in the metric's unit) never warns or fails, so tiny values
# don't trip over measurement noise.
METRICS = {
    "binary_kb": ("Binary size", "KB", "size", 32),
    "startup_ms": ("Startup, to window shown", "ms", "time", 15),
    "idle_rss_kb": ("Memory, empty document (RSS)", "KB", "memory", 512),
    "idle_heap_kb": ("Memory, empty document (own heap)", "KB", "memory", 256),
    "file30_load_ms": ("30 MB file, open until editable", "ms", "time", 150),
    "file30_rss_kb": ("Memory, 30 MB file open (RSS)", "KB", "memory", 1024),
    "file30_heap_kb": ("Memory, 30 MB file open (own heap)", "KB", "memory", 1024),
    "closed_heap_kb": ("Memory after closing it (own heap)", "KB", "memory", 256),
    "key_cpu_ms": ("CPU per keystroke, normal line", "ms", "time", 0.5),
    "key_long_cpu_ms": ("CPU per keystroke, 5M-character line", "ms", "time", 1.0),
}
# kind: (fail above ratio, warn above ratio)
TOLERANCE = {"size": (1.2, 1.05), "memory": (1.2, 1.1), "time": (3.0, 1.5)}


def sh(*args, **kw):
    return subprocess.run(args, capture_output=True, text=True, **kw)


def window(name):
    r = sh("xdotool", "search", "--name", name)
    ids = r.stdout.split()
    return ids[0] if ids else None


def wait(pred, timeout, step=0.005):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        v = pred()
        if v:
            return v
        time.sleep(step)
    raise TimeoutError(f"timed out after {timeout} s")


def status(pid):
    """VmRSS and RssAnon (the app's own memory: heap, stacks) in KB."""
    out = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        k, _, v = line.partition(":")
        if k in ("VmRSS", "RssAnon"):
            out[k] = int(v.split()[0])
    return out["VmRSS"], out["RssAnon"]


def cpu_ms(pid):
    f = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) * TICK_MS


class App:
    """One app instance with its own settings folder."""

    def __init__(self, *files):
        self.cfg = tempfile.mkdtemp(prefix="bench-cfg-")
        env = dict(os.environ, XDG_CONFIG_HOME=self.cfg, DISPLAY=DISPLAY)
        env.pop("WAYLAND_DISPLAY", None)
        self.start = time.monotonic()
        self.p = subprocess.Popen([str(APP), *map(str, files)], env=env, stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL)

    def focus(self, name):
        w = wait(lambda: window(name), 10)
        sh("xdotool", "windowsize", w, "1000", "640")
        sh("xdotool", "windowfocus", "--sync", w)
        time.sleep(0.3)
        return w

    def editable(self, name):
        """Waits until a typed character takes (big files load in the background)."""
        def typed():
            sh("xdotool", "key", "ctrl+Home")
            sh("xdotool", "type", "#")
            try:
                return wait(lambda: window(rf"\*{name}"), 0.5, 0.01)
            except TimeoutError:
                return None
        wait(typed, 20, 0)

    def close(self):
        self.p.kill()
        self.p.wait()
        shutil.rmtree(self.cfg, ignore_errors=True)


def measure(work):
    r = {"binary_kb": APP.stat().st_size // 1024}

    starts = []
    for _ in range(5):
        a = App()
        wait(lambda: window("Untitled - Plainpad"), 10)
        starts.append((time.monotonic() - a.start) * 1000)
        a.close()
    r["startup_ms"] = statistics.median(starts)

    a = App()
    a.focus("Untitled - Plainpad")
    time.sleep(1.5)
    r["idle_rss_kb"], r["idle_heap_kb"] = status(a.p.pid)
    # Typing on a short line.
    before = cpu_ms(a.p.pid)
    sh("xdotool", "type", "--delay", "20", "the quick brown fox jumps over the lazy dog " * 2)
    time.sleep(0.5)
    r["key_cpu_ms"] = (cpu_ms(a.p.pid) - before) / 88
    a.close()

    big = work / "big.txt"
    with big.open("w") as f:
        line = "The quick brown fox jumps over the lazy dog, https://example.com/path and more.\n"
        for _ in range(370_000):
            f.write(line)
    a = App(big)
    a.focus("big.txt")
    a.editable("big.txt")
    r["file30_load_ms"] = (time.monotonic() - a.start) * 1000
    time.sleep(1.5)
    r["file30_rss_kb"], r["file30_heap_kb"] = status(a.p.pid)
    sh("xdotool", "key", "ctrl+z")
    wait(lambda: window("^big.txt"), 5)
    sh("xdotool", "key", "ctrl+w")
    wait(lambda: window("^Untitled - Plainpad"), 5)
    time.sleep(1.5)
    r["closed_heap_kb"] = status(a.p.pid)[1]
    a.close()

    long = work / "long.txt"
    long.write_text("abc def; " * 600_000)
    a = App(long)
    a.focus("long.txt")
    a.editable("long.txt")
    sh("xdotool", "key", "ctrl+End")
    time.sleep(0.5)
    before = cpu_ms(a.p.pid)
    sh("xdotool", "type", "--delay", "20", "x" * 50)
    time.sleep(0.5)
    r["key_long_cpu_ms"] = (cpu_ms(a.p.pid) - before) / 50
    a.close()
    return {k: round(v, 2) for k, v in r.items()}


def compare(results, baseline):
    """Returns (rows, failed) with each row: name, baseline, now, change, verdict."""
    rows, failed = [], False
    for k, (desc, unit, kind, slack) in METRICS.items():
        now, base = results[k], baseline.get(k)
        if base is None:
            rows.append((desc, unit, None, now, "", "new"))
            continue
        fail_at, warn_at = TOLERANCE[kind]
        ratio = now / base if base else float("inf")
        if now > base * fail_at and now - base > slack:
            verdict, failed = "FAIL", True
        elif now > base * warn_at and now - base > slack:
            verdict = "warn"
        else:
            verdict = "ok"
        rows.append((desc, unit, base, now, f"{(ratio - 1) * 100:+.0f}%", verdict))
    return rows, failed


def report(rows):
    lines = ["| Metric | Baseline | Now | Change | |", "|---|---:|---:|---:|---|"]
    def fmt(v, unit):
        return "–" if v is None else f"{v:,.0f} {unit}" if v >= 100 else f"{v:.2f} {unit}"

    for desc, unit, base, now, change, verdict in rows:
        lines.append(f"| {desc} | {fmt(base, unit)} | {fmt(now, unit)} | {change} | {verdict} |")
    return "\n".join(lines)


def main():
    if not APP.exists():
        sys.exit(f"{APP} not found: cargo build --release first")
    xvfb = subprocess.Popen(["Xvfb", DISPLAY, "-screen", "0", "1280x800x24"], stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    os.environ["DISPLAY"] = DISPLAY
    work = Path(tempfile.mkdtemp(prefix="bench-"))
    try:
        wait(lambda: sh("xdotool", "getmouselocation").returncode == 0, 10, 0.1)
        results = measure(work)
    finally:
        xvfb.kill()
        shutil.rmtree(work, ignore_errors=True)

    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    if "--update" in sys.argv:
        BASELINE.write_text(json.dumps(results, indent=2) + "\n")
        print(f"baseline written to {BASELINE}")
    baseline = json.loads(BASELINE.read_text()) if BASELINE.exists() else {}
    rows, failed = compare(results, baseline)
    table = report(rows)
    print(table)
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as f:
            f.write("## Benchmarks\n\n" + table + "\n")
    if failed:
        sys.exit("benchmark regression: see FAIL rows (update the baseline with --update if intended)")


if __name__ == "__main__":
    main()
