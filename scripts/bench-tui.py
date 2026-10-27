#!/usr/bin/env python3
"""Reusable TUI benchmark harness and cluster-free regression gate.

Spawns a terminal UI in its own pseudo-terminal (with a controlling tty), answers
terminal queries like a real emulator (OSC 10/11 colors, DSR cursor position,
primary DA), waits for regex markers, then samples memory, CPU and (macOS only)
network for a fixed idle window. Every run gets a private, temporary HOME and
config directories, so the user's own settings are never read or written.

Subcommands:
  run      measure one command:   bench-tui.py run --name kborg --first api-7d4f9 -- kborg --demo
  gate     run fixed demo-mode scenarios; fail when a metric exceeds its budget or
           regresses more than 15 % against the committed per-OS/arch baseline
  check    re-evaluate a saved gate.json (e.g. a CI artifact) against the baseline,
           optionally storing it as the new baseline for its platform
  compare  KBorg vs k9s vs k10s against a live context, printed as a Markdown table

Standard library only. Pillow is needed only for --screenshots (PNG captures).
Unix only (macOS, Linux).
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import platform
import pty
import re
import runpy
import select
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time

SCRIPTS = Path(__file__).resolve().parent
REPO = SCRIPTS.parent
Screen = runpy.run_path(str(SCRIPTS / "test-terminal.py"))["Screen"]
WIDTH, HEIGHT = 130, 40
SYSTEM = platform.system()

# Metrics reduced by median across --repeat runs.
METRICS = ("first_data_seconds", "rss_mib_median", "rss_mib_peak", "rss_mib_max",
           "cpu_percent_avg", "net_kib_in_per_min", "net_kib_out_per_min",
           "key_latency_ms_p50", "key_latency_ms_p95", "key_latency_ms_p99")
# Checked against the absolute budgets in bench-budgets.json.
GATED = ("first_data_seconds", "rss_mib_median", "cpu_percent_avg")
# Checked against the committed baseline (lower is better for all of them).
# Step timings are added per scenario through "step_metrics".
COMPARED = ("first_data_seconds", "rss_mib_median", "rss_mib_max", "cpu_percent_avg",
            "key_latency_ms_p50", "key_latency_ms_p95", "key_latency_ms_p99")
DEFAULT_TOLERANCE = 15.0
# Absolute floors: a regression must exceed both the tolerance and the floor,
# so tiny values (a 2 ms latency, 0.1 % CPU) cannot flap on noisy runners.
DEFAULT_FLOORS = {"seconds": 0.05, "latency_ms": 3.0, "rss_mib": 4.0,
                  "cpu_percent": 1.0, "size_mib": 0.5}

# An explicit theme skips the first-run theme picker in the fresh config dir.
DEMO_ARGS = ["--demo", "--no-splash", "--theme", "flexoki-dark"]


def _scale_scenario(count):
    """KBORG_DEMO_SCALE adds synthetic Pods named scale-00000...: time load, a
    name filter down to one Pod, clearing it, and a reverse name sort, then
    measure key-to-frame latency with j/k on the full list."""
    target = f"scale-{count * 7 // 10:05d}"
    after = f"scale-{count * 7 // 10 + 1:05d}"
    return {"args": DEMO_ARGS, "first": r"scale-0000", "env": {"KBORG_DEMO_SCALE": str(count)},
            "latency": True,
            "steps": [[f"/{target}\\r", r"\[1/1\] FILTERED"],
                      ["\\x1b", r"pods \[\d+/\d{4,}\]"],
                      # Selection stays on target; descending puts `after` above it.
                      ["N", after + r".*\n.*" + target]],
            "step_metrics": ["filter_seconds", "clear_filter_seconds", "sort_seconds"]}


GATE_SCENARIOS = {
    "demo-startup": {"args": DEMO_ARGS, "first": r"api-7d4f9", "steps": [], "latency": True},
    # The scale scenarios are required; a missing marker is a regression.
    "scale-5k": _scale_scenario(5000),
    "scale-20k": _scale_scenario(20000),
    # D is ignored while the startup metrics request is busy: wait for it to
    # finish, then press D (re-sent every 2 s as a safety net on slow runners).
    "demo-dashboard": {"args": DEMO_ARGS, "first": r"api-7d4f9",
                       "steps": [["", r"METRICS [0-9]+ measured"],
                                 ["D", r"CONTEXT OVERVIEW", 2.0]],
                       "step_metrics": [None, "dashboard_seconds"]},
}

ESCAPES = re.compile(rb"\x1b(\[[0-9;?<>=]*[ -/]*[@-~]|\][^\x07\x1b]*(\x07|\x1b\\)|[()][A-Z0-9]|[=>78])")


class MarkerMissing(Exception):
    def __init__(self, marker, tail, screen=""):
        super().__init__(f"marker {marker!r} did not appear; output tail: {tail}")
        self.marker, self.screen = marker, screen


# ---------------------------------------------------------------- sampling

def _ps_cputime(text):
    """Parse ps TIME: [[dd-]hh:]mm:ss[.cc]."""
    days = 0
    if "-" in text:
        day_text, text = text.split("-", 1)
        days = int(day_text)
    seconds = 0.0
    for part in text.strip().split(":"):
        seconds = seconds * 60 + float(part)
    return days * 86400 + seconds


def sample(pid):
    """(rss_kib, cpu_seconds) for pid, or None if it is gone."""
    if SYSTEM == "Linux":
        try:
            stat = Path(f"/proc/{pid}/stat").read_text()
            fields = stat.rsplit(")", 1)[1].split()
            ticks = os.sysconf("SC_CLK_TCK")
            cpu = (int(fields[11]) + int(fields[12])) / ticks  # utime + stime
            for line in Path(f"/proc/{pid}/status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]), cpu
        except (OSError, IndexError, ValueError):
            return None
        return None
    try:
        text = subprocess.check_output(["ps", "-o", "rss=,time=", "-p", str(pid)],
                                       text=True, stderr=subprocess.DEVNULL)
        rss, cpu = text.split()
        return int(rss), _ps_cputime(cpu)
    except (subprocess.CalledProcessError, ValueError, OSError):
        return None


def network(pid):
    """Cumulative (bytes_in, bytes_out) for pid via nettop, or None.

    Only macOS exposes per-process socket counters without root; on Linux
    /proc/PID/io mixes terminal and file I/O with sockets, so network is skipped.
    """
    if SYSTEM != "Darwin" or not shutil.which("nettop"):
        return None
    try:
        text = subprocess.check_output(
            ["nettop", "-P", "-L", "1", "-x", "-p", str(pid), "-J", "bytes_in,bytes_out"],
            text=True, stderr=subprocess.DEVNULL, timeout=10)
    except (subprocess.SubprocessError, OSError):
        return None
    lines = [line for line in text.splitlines() if line.strip()]
    if not lines:
        return None
    header = lines[0].split(",")
    try:
        col_in, col_out = header.index("bytes_in"), header.index("bytes_out")
    except ValueError:
        return None
    total_in = total_out = 0
    for row in lines[1:]:
        cells = row.split(",")
        try:
            total_in += int(cells[col_in] or 0)
            total_out += int(cells[col_out] or 0)
        except (IndexError, ValueError):
            continue
    return total_in, total_out


# ---------------------------------------------------------------- one run

def _isolated_env(home, kubeconfig, extra):
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
        "TERM": "xterm-256color",
        "COLORTERM": "truecolor",
        "LANG": "en_US.UTF-8",
        "KBORG_CONFIG_DIR": str(home / "kborg"),
        "K9S_CONFIG_DIR": str(home / "k9s"),
        "XDG_CONFIG_HOME": str(home / "config"),
        "XDG_DATA_HOME": str(home / "data"),
        "XDG_STATE_HOME": str(home / "state"),
        "XDG_CACHE_HOME": str(home / "cache"),
    }
    if kubeconfig:
        env["KUBECONFIG"] = os.pathsep.join(str(Path(p).expanduser().resolve())
                                            for p in kubeconfig.split(os.pathsep))
    env.update(extra or {})
    return env


def _spawn(argv, env, slave):
    """Give the binary its own session and controlling terminal, without a shell."""
    def setup():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    return subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave,
                            env=env, preexec_fn=setup, close_fds=True)


def _signal(proc, sig):
    for send in (lambda: os.killpg(proc.pid, sig), lambda: proc.send_signal(sig)):
        try:
            send()
            return
        except (ProcessLookupError, PermissionError):
            continue


def _reap(proc):
    """Reap proc with wait4 so its resource usage is kept; None while it runs."""
    if proc.returncode is not None:
        return getattr(proc, "rusage", None) or False
    try:
        pid, status, usage = os.wait4(proc.pid, os.WNOHANG)
    except ChildProcessError:
        proc.returncode = -1
        return False
    if pid == 0:
        return None
    proc.returncode = os.waitstatus_to_exitcode(status)
    proc.rusage = usage
    return usage


def _peak_rss_mib(usage):
    """ru_maxrss is bytes on macOS and KiB on Linux."""
    if not usage:
        return None
    return round(usage.ru_maxrss / (2**20 if SYSTEM == "Darwin" else 2**10), 1)


def _stop(proc, drain):
    """SIGTERM, keep draining the PTY so terminal restore cannot block, then
    SIGKILL. Returns the child's rusage (for the peak RSS) when available."""
    for sig, grace in ((signal.SIGTERM, 5), (signal.SIGKILL, 5)):
        if _reap(proc) is not None:
            return _reap(proc)
        _signal(proc, sig)
        deadline = time.monotonic() + grace
        while _reap(proc) is None and time.monotonic() < deadline:
            drain(0.1)
            time.sleep(0.02)
    return _reap(proc)


def percentile(values, pct):
    """Nearest-rank percentile of a non-empty list."""
    ordered = sorted(values)
    rank = max(1, -(-len(ordered) * pct // 100))
    return ordered[int(rank) - 1]


def measure(name, argv, first, steps=(), seconds=10, kubeconfig=None, env=None,
            timeout=60, out=None, screenshots=False, latency_samples=0):
    """Run argv once and return a result dict. Raises MarkerMissing.

    With latency_samples, after the idle window it alternately sends j and k and
    times each key until the emulated screen (text or colors, after any
    synchronized update completes) differs from the frame before the key."""
    binary = shutil.which(argv[0]) or argv[0]
    argv = [binary] + list(argv[1:])
    result = {"tool": name, "argv": argv, "binary": binary,
              "binary_mib": round(os.path.getsize(binary) / 2**20, 1) if os.path.isfile(binary) else None,
              "platform": f"{SYSTEM} {platform.machine()}", "idle_seconds": seconds}
    with tempfile.TemporaryDirectory(prefix=f"bench-{name}-") as tmp:
        home = Path(tmp).resolve()
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", HEIGHT, WIDTH, 0, 0))
        screen = Screen(WIDTH, HEIGHT)
        raw = bytearray()
        started = time.monotonic()
        proc = _spawn(argv, _isolated_env(home, kubeconfig, env), slave)

        def pump(wait_seconds):
            """Read and process one chunk; False on timeout, None on EOF."""
            if not select.select([master], [], [], wait_seconds)[0]:
                return False
            try:
                data = os.read(master, 1 << 16)
            except OSError:
                return None
            if not data:
                return None
            raw.extend(data)
            if len(raw) > 1 << 20:
                del raw[:len(raw) - (1 << 19)]
            if b"\x1b]11;?" in data:
                os.write(master, b"\x1b]11;rgb:1010/0f0f/0f0f\x1b\\")
            if b"\x1b]10;?" in data:
                os.write(master, b"\x1b]10;rgb:cece/cdcd/c3c3\x1b\\")
            if b"\x1b[6n" in data:
                os.write(master, b"\x1b[1;1R")
            if b"\x1b[c" in data or b"\x1b[0c" in data:
                os.write(master, b"\x1b[?62;22c")
            try:
                screen.feed(data)
            except Exception:  # emulator is best effort; the raw stream is also searched
                pass
            return True

        def drain(duration=0.05):
            end = time.monotonic() + duration
            while time.monotonic() < end:
                if pump(0.02) is None:
                    return

        def key_latency(samples):
            """Milliseconds from each key write to the first changed frame."""
            times, timeouts = [], 0
            for index in range(samples):
                quiet_since, settle_end = time.monotonic(), time.monotonic() + 1.0
                while time.monotonic() < settle_end and time.monotonic() - quiet_since < 0.05:
                    if pump(0.01):
                        quiet_since = time.monotonic()
                before = [row[:] for row in screen.visible_cells()]
                started_key = time.monotonic()
                send("jk"[index % 2])
                while time.monotonic() - started_key < 1.0:
                    if pump(0.005) is None:
                        break
                    if screen.visible_cells() != before:
                        times.append((time.monotonic() - started_key) * 1000)
                        break
                else:
                    timeouts += 1
            return times, timeouts

        def send(keys):
            os.write(master, keys.encode("utf-8").decode("unicode_escape").encode("latin-1"))

        def wait(marker, since, keys="", resend=0):
            raw.clear()
            pattern = re.compile(marker)
            last_send = since
            while time.monotonic() - since < timeout:
                # Check the emulated screen after every chunk so step timings are
                # not rounded up to a polling interval; the slower raw-stream
                # search runs only once output pauses.
                got = pump(0.02)
                if resend and keys and time.monotonic() - last_send >= resend:
                    send(keys)  # opt-in: the app may ignore keys while a request is busy
                    last_send = time.monotonic()
                if got and pattern.search(screen.text()):
                    return time.monotonic() - since
                if not got and pattern.search(ESCAPES.sub(b"", bytes(raw)).decode("utf-8", "replace")):
                    return time.monotonic() - since
                if proc.poll() is not None:
                    break
            tail = " ".join(ESCAPES.sub(b" ", bytes(raw)).decode("utf-8", "replace").split())
            raise MarkerMissing(marker, tail[-600:] or "(no output)", screen.text())

        try:
            result["first_data_seconds"] = round(wait(first, started), 3)
            result["steps"] = []
            for keys, marker, *rest in steps:
                since = time.monotonic()
                send(keys)
                step_seconds = wait(marker, since, keys, rest[0] if rest else 0)
                result["steps"].append({"keys": keys, "marker": marker, "seconds": round(step_seconds, 3)})
            drain(2)
            base, net_base = sample(proc.pid), network(proc.pid)
            window_start = time.monotonic()
            rss = []
            while time.monotonic() - window_start < seconds:
                drain(min(1.0, max(0, seconds - (time.monotonic() - window_start))))
                point = sample(proc.pid)
                if point:
                    rss.append(point[0])
            last, net_last = sample(proc.pid), network(proc.pid)
            window = time.monotonic() - window_start
            result["sampled_seconds"] = round(window, 3)
            result["sample_count"] = len(rss)
            if not (base and last and rss):
                raise RuntimeError(f"{name}: process exited during sampling")
            result["rss_mib_median"] = round(statistics.median(rss) / 1024, 1)
            result["rss_mib_peak"] = round(max(rss) / 1024, 1)
            result["cpu_percent_avg"] = round(100 * (last[1] - base[1]) / window, 2)
            if net_base and net_last:
                result["net_kib_in_per_min"] = round((net_last[0] - net_base[0]) / 1024 * 60 / window, 1)
                result["net_kib_out_per_min"] = round((net_last[1] - net_base[1]) / 1024 * 60 / window, 1)
            else:
                result["network"] = "unavailable on this platform"
            if latency_samples:
                times, timeouts = key_latency(latency_samples)
                if not times:
                    raise RuntimeError(f"{name}: no frame changed after {latency_samples} keys")
                _latency_summary(result, [round(t, 3) for t in times])
                result["key_latency_timeouts"] = timeouts
            result["total_wall_seconds"] = round(time.monotonic() - started, 1)
            if out:
                out.mkdir(parents=True, exist_ok=True)
                (out / f"{name}.txt").write_text(screen.text())
                if screenshots:
                    screen.png(out / f"{name}.png")
        finally:
            peak = _peak_rss_mib(_stop(proc, drain))
            os.close(master)
            os.close(slave)
    if peak is not None:
        result["rss_mib_max"] = peak  # whole-run high-water mark (getrusage)
    return result


def _latency_summary(result, times):
    result["key_latency_ms"] = times
    for pct in (50, 95, 99):
        result[f"key_latency_ms_p{pct}"] = round(percentile(times, pct), 2)
    result["key_latency_samples"] = len(times)


def _is_timing(metric):
    return metric.endswith("_seconds") or metric.startswith("key_latency_ms_p")


def measure_repeated(repeat, *args, **kwargs):
    """Reduce `repeat` runs to one result; the individual runs are kept.

    Timings (first data, steps, key latency percentiles) take the best run:
    noisy neighbours on a shared runner only ever add time, while a real
    slowdown raises every run. Memory and CPU take the median."""
    runs = [measure(*args, **kwargs) for _ in range(repeat)]
    if repeat == 1:
        return runs[0]
    merged = dict(runs[0])
    for metric in METRICS:
        values = [r[metric] for r in runs if metric in r]
        if values:
            merged[metric] = round(min(values) if _is_timing(metric) else statistics.median(values), 3)
    merged["steps"] = [dict(step, seconds=round(min(r["steps"][index]["seconds"] for r in runs), 3))
                       for index, step in enumerate(runs[0].get("steps", []))]
    merged.pop("key_latency_ms", None)
    if any("key_latency_timeouts" in r for r in runs):
        merged["key_latency_timeouts"] = sum(r.get("key_latency_timeouts", 0) for r in runs)
    merged["repeat"] = repeat
    merged["runs"] = [{m: r.get(m) for m in METRICS if m in r} for r in runs]
    return merged


# ---------------------------------------------------------------- commands

def _parse_steps(pairs, resend=0):
    return [list(pair) + ([resend] if resend else []) for pair in (pairs or [])]


def cmd_run(args):
    if not args.argv:
        sys.exit("run: missing command after --")
    out = Path(args.out).resolve() if args.out else None
    try:
        result = measure_repeated(args.repeat, args.name, args.argv, args.first,
                                  _parse_steps(args.step, args.resend), args.seconds, args.kubeconfig,
                                  dict(e.split("=", 1) for e in args.env), args.timeout,
                                  out, args.screenshots)
    except MarkerMissing as error:
        sys.exit(f"{args.name}: {error}")
    text = json.dumps(result, indent=2)
    if out:
        (out / f"{args.name}.json").write_text(text + "\n")
    print(text)


def _table(rows, headers):
    widths = [max(len(str(x)) for x in col) for col in zip(headers, *rows)]
    line = lambda cells: "| " + " | ".join(str(c).ljust(w) for c, w in zip(cells, widths)) + " |"
    return "\n".join([line(headers), "|" + "|".join("-" * (w + 2) for w in widths) + "|"]
                     + [line(r) for r in rows])


def platform_key():
    """Baseline key, e.g. Darwin-arm64 or Linux-x86_64."""
    return f"{SYSTEM}-{platform.machine()}"


def _machine():
    try:
        if SYSTEM == "Darwin":
            return subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"],
                                           text=True, stderr=subprocess.DEVNULL).strip()
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.lower().startswith(("model name", "cpu model")):
                return line.split(":", 1)[1].strip()
    except (OSError, subprocess.SubprocessError):
        pass
    return platform.processor() or platform.machine()


def _commit():
    try:
        return subprocess.check_output(["git", "-C", str(REPO), "rev-parse", "--short", "HEAD"],
                                       text=True, stderr=subprocess.DEVNULL).strip()
    except (OSError, subprocess.SubprocessError):
        return None


def flat_metrics(doc):
    """{scenario: {metric: value}} from a gate results document, including
    binary size under the pseudo-scenario "binary"."""
    metrics = {"binary": {"size_mib": doc["binary_mib"]}} if doc.get("binary_mib") else {}
    for name, result in doc.get("scenarios", {}).items():
        if result.get("status") in ("fail", "skipped"):
            continue
        values = {m: result[m] for m in COMPARED if result.get(m) is not None}
        for step in result.get("steps", []):
            if step.get("metric"):
                values[step["metric"]] = step["seconds"]
        metrics[name] = values
    return metrics


def floor_for(metric, floors):
    if metric.startswith("key_latency_ms"):
        return floors["latency_ms"]
    if metric.startswith("rss_mib"):
        return floors["rss_mib"]
    for key in ("seconds", "cpu_percent", "size_mib"):
        if key in metric:
            return floors[key]
    return 0.0


def evaluate(doc, budgets, baseline, tolerance):
    """Rows for the delta table and whether the gate failed.

    A metric fails when it exceeds its absolute budget, or when it is more than
    `tolerance` percent AND more than its absolute floor above the baseline."""
    floors = dict(DEFAULT_FLOORS, **baseline.get("floors", {}))
    base = baseline.get("platforms", {}).get(doc["platform"], {}).get("metrics", {})
    rows, failed = [], False
    for name, result in doc.get("scenarios", {}).items():
        if result.get("status") == "skipped":
            rows.append([name, "-", "-", "-", "-", "-", "SKIPPED (feature not in this binary)"])
        elif result.get("status") == "fail":
            reason = " ".join(result.get("reason", "").split())[:120]
            rows.append([name, "measurement", "-", "-", "-", "-", f"FAIL: {reason}"])
            failed = True
    for name, values in flat_metrics(doc).items():
        budget = budgets.get(name, {}).get("budget", {})
        for metric, value in values.items():
            reference = base.get(name, {}).get(metric)
            limit = budget.get(metric)
            notes = []
            if limit is not None and value > limit:
                notes.append(f"FAIL: over budget {limit}")
            if reference is None:
                delta = "-"
                notes.append("new (no baseline)")
            else:
                diff = value - reference
                delta = f"{100 * diff / reference:+.1f} %" if reference else f"{diff:+g}"
                floor = floor_for(metric, floors)
                if diff > floor and diff > reference * tolerance / 100:
                    notes.append(f"FAIL: > {tolerance:g} % regression")
                elif -diff > floor and -diff > reference * tolerance / 100:
                    notes.append("better (consider --update-baseline)")
            if any(note.startswith("FAIL") for note in notes):
                failed = True
            rows.append([name, metric, reference if reference is not None else "-", value, delta,
                         limit if limit is not None else "-", "; ".join(notes) or "ok"])
    for name, values in base.items():  # a metric that vanished is a broken scenario
        for metric in values:
            if name in doc.get("scenarios", {}) and metric not in flat_metrics(doc).get(name, {}):
                status = doc["scenarios"][name].get("status")
                if status not in ("fail", "skipped"):
                    rows.append([name, metric, values[metric], "-", "-", "-", "FAIL: not measured"])
                    failed = True
    return rows, failed


def update_baseline(path, doc):
    """Store doc's metrics as the baseline for doc's platform; other platforms
    are kept. Refuses when any scenario failed to measure."""
    if any(r.get("status") == "fail" for r in doc.get("scenarios", {}).values()):
        sys.exit("baseline: refusing to update from a run with failed scenarios")
    path = Path(path)
    baseline = json.loads(path.read_text()) if path.exists() else {
        "schema": 1, "tolerance_percent": DEFAULT_TOLERANCE, "floors": DEFAULT_FLOORS,
        "platforms": {}}
    platforms = baseline.setdefault("platforms", {})
    entry = {
        "recorded": doc.get("date"), "commit": doc.get("commit"), "machine": doc.get("machine"),
        "repeat": doc.get("repeat"), "idle_seconds": doc.get("seconds"),
        "binary_bytes": doc.get("binary_bytes"), "metrics": flat_metrics(doc)}
    # A re-recorded platform keeps its own tolerance.
    if "tolerance_percent" in platforms.get(doc["platform"], {}):
        entry["tolerance_percent"] = platforms[doc["platform"]]["tolerance_percent"]
    platforms[doc["platform"]] = entry
    path.write_text(json.dumps(baseline, indent=2) + "\n")
    print(f"baseline: wrote {doc['platform']} to {path}")


def report(doc, rows, failed, tolerance, out):
    """Print the delta table and write it as Markdown to OUT/summary.md and,
    in GitHub Actions, to the job summary."""
    table = _table(rows, ["scenario", "metric", "baseline", "value", "delta", "budget", "status"])
    print(table)
    verdict = "FAILED" if failed else "passed"
    summary = (f"## Performance gate: {verdict}\n\n"
               f"Platform `{doc['platform']}` ({doc.get('machine')}), commit `{doc.get('commit')}`, "
               f"median of {doc.get('repeat')} run(s), {doc.get('seconds')} s idle window. "
               f"Fails on more than {tolerance:g} % regression against `scripts/bench-baseline.json` "
               f"(and beyond an absolute floor) or on exceeding `scripts/bench-budgets.json`.\n\n"
               f"{table}\n")
    if out:
        out.mkdir(parents=True, exist_ok=True)
        (out / "summary.md").write_text(summary)
    step_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary:
        with open(step_summary, "a", encoding="utf-8") as handle:
            handle.write(summary)
    print(f"gate: {verdict}")


def _finish(args, doc):
    path = Path(args.baseline)
    baseline = json.loads(path.read_text()) if path.exists() else {}
    # --tolerance, else the platform's own tolerance_percent (shared CI
    # runners are noisier than a quiet workstation), else the file-wide one.
    platform = baseline.get("platforms", {}).get(doc["platform"], {})
    tolerance = args.tolerance if args.tolerance is not None else platform.get(
        "tolerance_percent", baseline.get("tolerance_percent", DEFAULT_TOLERANCE))
    budgets = json.loads(Path(args.budgets).read_text())["scenarios"]
    if doc["platform"] not in baseline.get("platforms", {}):
        print(f"gate: no baseline for {doc['platform']} in {path}; only budgets are checked. "
              f"Store this run with: bench-tui.py check --results gate.json --update-baseline",
              file=sys.stderr)
    rows, failed = evaluate(doc, budgets, baseline, tolerance)
    report(doc, rows, failed, tolerance, Path(args.out).resolve() if args.out else None)
    if args.update_baseline:
        update_baseline(path, doc)
        return 0
    return 1 if failed else 0


def cmd_gate(args):
    binary = str(Path(args.binary).resolve())
    if not os.access(binary, os.X_OK):
        sys.exit(f"gate: binary not executable: {binary}")
    size = os.path.getsize(binary)
    doc = {"schema": 2, "platform": platform_key(), "machine": _machine(), "commit": _commit(),
           "date": time.strftime("%Y-%m-%d"), "repeat": args.repeat, "seconds": args.seconds,
           "binary_bytes": size, "binary_mib": round(size / 2**20, 2), "scenarios": {}}
    out = Path(args.out).resolve() if args.out else None
    for name in args.only or list(GATE_SCENARIOS):
        scenario = GATE_SCENARIOS[name]
        samples = args.latency_samples if scenario.get("latency") else 0
        print(f"gate: running {name} ({args.repeat}x, {args.seconds}s idle, "
              f"{samples} latency keys)...", file=sys.stderr, flush=True)
        try:
            result = measure_repeated(args.repeat, name, [binary] + scenario["args"],
                                      scenario["first"], scenario["steps"], args.seconds,
                                      None, scenario.get("env"), args.timeout, out,
                                      latency_samples=samples)
        except (MarkerMissing, RuntimeError) as error:
            skip = isinstance(error, MarkerMissing) and scenario.get("skip_if_missing")
            doc["scenarios"][name] = {"status": "skipped" if skip else "fail", "reason": str(error)}
            if not skip:
                print(f"gate: {name}: {error}", file=sys.stderr)
            continue
        for step, metric in zip(result.get("steps", []), scenario.get("step_metrics", [])):
            if metric:
                step["metric"] = metric
        doc["scenarios"][name] = result
    if out:
        out.mkdir(parents=True, exist_ok=True)
        (out / "gate.json").write_text(json.dumps(doc, indent=2) + "\n")
    sys.exit(_finish(args, doc))


def cmd_check(args):
    """Re-evaluate a saved gate.json, e.g. the bench-results CI artifact."""
    doc = json.loads(Path(args.results).read_text())
    if doc.get("schema") != 2:
        sys.exit("check: results must come from `gate --out DIR` (schema 2)")
    sys.exit(_finish(args, doc))


def cmd_compare(args):
    """KBorg and k9s get the same --first marker and --step keys; k10s (a
    multi-cluster dashboard with different navigation) gets only its own
    first marker (--k10s-first, default --first)."""
    context, ns = args.context, args.namespace
    steps = _parse_steps(args.step, args.resend)
    tools = [("KBorg", [args.kborg, "--context", context, "-n", ns, "--no-splash",
                       "--theme", "flexoki-dark"], args.first, steps)]
    if args.k9s and shutil.which(args.k9s):
        tools.append(("k9s", [args.k9s, "--context", context, "-n", ns, "--splashless"], args.first, steps))
    if args.k10s and shutil.which(args.k10s):
        tools.append(("k10s", [args.k10s, "-c", context], args.k10s_first or args.first, []))
    out = Path(args.out).resolve() if args.out else None
    results = {}
    for label, argv, first, tool_steps in tools:
        print(f"compare: {label}...", file=sys.stderr, flush=True)
        try:
            results[label] = measure_repeated(args.repeat, label.lower(), argv, first, tool_steps,
                                              args.seconds, args.kubeconfig, None, args.timeout, out)
        except MarkerMissing as error:
            print(f"compare: {label}: {error}", file=sys.stderr)
            results[label] = {}
    labels = list(results)
    rows = [["Binary (MiB)"] + [results[l].get("binary_mib", "-") for l in labels],
            ["Time to first data (s)"] + [results[l].get("first_data_seconds", "-") for l in labels]]
    for index, (keys, _) in enumerate(args.step or []):
        rows.append([f"Step {keys!r} (s)"] + [
            (results[l].get("steps") or [{}] * (index + 1))[index].get("seconds", "-") for l in labels])
    for title, metric in (("Memory, median (MiB)", "rss_mib_median"), ("Memory, peak (MiB)", "rss_mib_peak"),
                          ("CPU, average (%)", "cpu_percent_avg"),
                          ("Network in (KiB/min)", "net_kib_in_per_min"),
                          ("Network out (KiB/min)", "net_kib_out_per_min")):
        rows.append([title] + [results[l].get(metric, "-") for l in labels])
    print(_table(rows, ["Metric"] + labels))
    if out:
        out.mkdir(parents=True, exist_ok=True)
        (out / "compare.json").write_text(json.dumps(results, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    def common(p, seconds):
        p.add_argument("--seconds", type=float, default=seconds, help="idle sampling window")
        p.add_argument("--repeat", type=int, default=1, help="runs per scenario; metrics are medians")
        p.add_argument("--timeout", type=float, default=60, help="seconds to wait for each marker")
        p.add_argument("--out", help="directory for JSON results and final screen text")
        p.add_argument("--resend", type=float, default=0, metavar="SECONDS",
                       help="re-send each --step's keys every SECONDS until its marker shows (default off)")

    run = sub.add_parser("run", help="measure one command")
    run.add_argument("--name", required=True)
    run.add_argument("--first", required=True, help="regex marking first data on screen")
    run.add_argument("--step", nargs=2, action="append", metavar=("KEYS", "MARKER"),
                     help="keys to send (\\r, \\x1b escapes allowed), then a regex to wait for")
    run.add_argument("--kubeconfig", help="KUBECONFIG for the child (default: none, isolated HOME)")
    run.add_argument("--env", action="append", default=[], metavar="NAME=VALUE")
    run.add_argument("--screenshots", action="store_true", help="also write a PNG (needs Pillow)")
    common(run, 60)
    run.add_argument("argv", nargs=argparse.REMAINDER)
    run.set_defaults(func=cmd_run)

    def baseline_args(p):
        p.add_argument("--budgets", default=str(SCRIPTS / "bench-budgets.json"))
        p.add_argument("--baseline", default=str(SCRIPTS / "bench-baseline.json"),
                       help="per-OS/arch baseline file (default scripts/bench-baseline.json)")
        p.add_argument("--tolerance", type=float, metavar="PERCENT",
                       help="allowed regression (default: the baseline file's tolerance_percent, 15)")
        p.add_argument("--update-baseline", action="store_true",
                       help="store these results as the baseline for their platform and exit 0")

    gate = sub.add_parser("gate", help="cluster-free demo-mode regression gate")
    baseline_args(gate)
    gate.add_argument("--binary", default=str(REPO / "target/release/kborg"))
    gate.add_argument("--only", action="append", choices=list(GATE_SCENARIOS))
    gate.add_argument("--latency-samples", type=int, default=100, metavar="N",
                      help="j/k keys timed per run for key-to-frame latency (default 100)")
    common(gate, 10)
    gate.set_defaults(func=cmd_gate, timeout=30)

    check = sub.add_parser("check", help="evaluate a saved gate.json against the baseline")
    check.add_argument("--results", required=True, help="gate.json written by `gate --out DIR`")
    check.add_argument("--out", help="directory for summary.md")
    baseline_args(check)
    check.set_defaults(func=cmd_check)

    compare = sub.add_parser("compare", help="KBorg vs k9s vs k10s on a live context")
    compare.add_argument("--context", required=True)
    compare.add_argument("--namespace", "-n", default="kube-system")
    compare.add_argument("--kubeconfig")
    compare.add_argument("--first", required=True, help="regex present in every tool once data shows")
    compare.add_argument("--step", nargs=2, action="append", metavar=("KEYS", "MARKER"))
    compare.add_argument("--kborg", default=str(REPO / "target/release/kborg"))
    compare.add_argument("--k9s", default="k9s")
    compare.add_argument("--k10s", default="k10s")
    compare.add_argument("--k10s-first", help="first-data regex for k10s (default: --first)")
    common(compare, 60)
    compare.set_defaults(func=cmd_compare)

    args = parser.parse_args()
    if getattr(args, "argv", None) and args.argv[0] == "--":
        args.argv = args.argv[1:]
    args.func(args)


if __name__ == "__main__":
    main()
