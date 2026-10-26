#!/usr/bin/env python3
"""Reusable TUI benchmark harness and cluster-free regression gate.

Spawns a terminal UI in its own pseudo-terminal (with a controlling tty), answers
terminal queries like a real emulator (OSC 10/11 colors, DSR cursor position,
primary DA), waits for regex markers, then samples memory, CPU and (macOS only)
network for a fixed idle window. Every run gets a private, temporary HOME and
config directories, so the user's own settings are never read or written.

Subcommands:
  run      measure one command:   bench-tui.py run --name kborg --first api-7d4f9 -- kborg --demo
  gate     run fixed demo-mode scenarios and fail when a metric exceeds its budget
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

# Metrics reduced by median across --repeat runs and checked by the gate.
METRICS = ("first_data_seconds", "rss_mib_median", "rss_mib_peak", "cpu_percent_avg",
           "net_kib_in_per_min", "net_kib_out_per_min")
GATED = ("first_data_seconds", "rss_mib_median", "cpu_percent_avg")

# An explicit theme skips the first-run theme picker in the fresh config dir.
DEMO_ARGS = ["--demo", "--no-splash", "--theme", "flexoki-dark"]
GATE_SCENARIOS = {
    "demo-startup": {"args": DEMO_ARGS, "first": r"api-7d4f9", "steps": []},
    # The 5k scale scenario is required; a missing marker is a regression.
    "demo-scale": {"args": DEMO_ARGS, "first": r"scale-0000", "steps": [],
                   "env": {"KBORG_DEMO_SCALE": "5000"}},
    # D is ignored while the startup metrics request is busy: wait for it to
    # finish, then press D (re-sent every 2 s as a safety net on slow runners).
    "demo-dashboard": {"args": DEMO_ARGS, "first": r"api-7d4f9",
                       "steps": [["", r"METRICS [0-9]+ measured"],
                                 ["D", r"CONTEXT OVERVIEW", 2.0]]},
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


def _stop(proc, drain):
    """SIGTERM, keep draining the PTY so terminal restore cannot block, then SIGKILL."""
    for sig, grace in ((signal.SIGTERM, 5), (signal.SIGKILL, 5)):
        if proc.poll() is not None:
            return
        _signal(proc, sig)
        deadline = time.monotonic() + grace
        while proc.poll() is None and time.monotonic() < deadline:
            drain(0.1)
            time.sleep(0.02)
    proc.wait(1)


def measure(name, argv, first, steps=(), seconds=10, kubeconfig=None, env=None,
            timeout=60, out=None, screenshots=False):
    """Run argv once and return a result dict. Raises MarkerMissing."""
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

        def drain(duration=0.05):
            end = time.monotonic() + duration
            while time.monotonic() < end:
                if not select.select([master], [], [], 0.02)[0]:
                    continue
                try:
                    data = os.read(master, 1 << 16)
                except OSError:
                    return
                if not data:
                    return
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

        def send(keys):
            os.write(master, keys.encode("utf-8").decode("unicode_escape").encode("latin-1"))

        def wait(marker, since, keys="", resend=0):
            raw.clear()
            pattern = re.compile(marker)
            last_send = since
            while time.monotonic() - since < timeout:
                # Short slices: a marker is noticed within ~10 ms, not 50 ms.
                drain(0.01)
                if resend and keys and time.monotonic() - last_send >= resend:
                    send(keys)  # opt-in: the app may ignore keys while a request is busy
                    last_send = time.monotonic()
                plain = ESCAPES.sub(b"", bytes(raw)).decode("utf-8", "replace")
                if pattern.search(screen.text()) or pattern.search(plain):
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
            result["total_wall_seconds"] = round(time.monotonic() - started, 1)
            if out:
                out.mkdir(parents=True, exist_ok=True)
                (out / f"{name}.txt").write_text(screen.text())
                if screenshots:
                    screen.png(out / f"{name}.png")
        finally:
            _stop(proc, drain)
            os.close(master)
            os.close(slave)
    return result


def measure_repeated(repeat, *args, **kwargs):
    """Median of each metric across `repeat` runs; the individual runs are kept."""
    runs = [measure(*args, **kwargs) for _ in range(repeat)]
    if repeat == 1:
        return runs[0]
    merged = dict(runs[0])
    for metric in METRICS:
        values = [r[metric] for r in runs if metric in r]
        if values:
            merged[metric] = round(statistics.median(values), 3)
    for index, step in enumerate(merged.get("steps", [])):
        merged["steps"][index] = dict(step, seconds=round(statistics.median(
            r["steps"][index]["seconds"] for r in runs), 3))
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


def cmd_gate(args):
    budgets = json.loads(Path(args.budgets).read_text())["scenarios"]
    binary = str(Path(args.binary).resolve())
    if not os.access(binary, os.X_OK):
        sys.exit(f"gate: binary not executable: {binary}")
    selected = args.only or list(GATE_SCENARIOS)
    rows, failed, results = [], False, {}
    for name in selected:
        scenario = GATE_SCENARIOS[name]
        budget = budgets.get(name, {}).get("budget", {})
        print(f"gate: running {name} ({args.repeat}x, {args.seconds}s idle)...", file=sys.stderr, flush=True)
        try:
            result = measure_repeated(args.repeat, name, [binary] + scenario["args"],
                                      scenario["first"], scenario["steps"], args.seconds,
                                      None, scenario.get("env"), args.timeout,
                                      Path(args.out).resolve() if args.out else None)
        except MarkerMissing as error:
            rows.append([name, "first marker", "missing", "-", "FAIL"])
            results[name] = {"status": "fail", "reason": str(error)}
            print(f"gate: {name}: {error}", file=sys.stderr)
            failed = True
            continue
        results[name] = result
        for metric in GATED:
            value, limit = result.get(metric), budget.get(metric)
            if limit is None:
                status = "no budget"
            elif value is None:
                status, failed = "FAIL (not measured)", True
            elif value > limit:
                status, failed = "FAIL", True
            else:
                status = "ok"
            rows.append([name, metric, value, limit if limit is not None else "-", status])
        for step in result.get("steps", []):
            label = f"step {step['keys']!r} -> /{step['marker']}/ (s)" if step["keys"] else f"wait /{step['marker']}/ (s)"
            rows.append([name, label, step["seconds"], "-", "info"])
    print(_table(rows, ["scenario", "metric", "value", "budget", "status"]))
    if args.out:
        Path(args.out).mkdir(parents=True, exist_ok=True)
        (Path(args.out) / "gate.json").write_text(json.dumps(results, indent=2) + "\n")
    print("gate: FAILED" if failed else "gate: passed")
    sys.exit(1 if failed else 0)


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

    gate = sub.add_parser("gate", help="cluster-free demo-mode regression gate")
    gate.add_argument("--budgets", default=str(SCRIPTS / "bench-budgets.json"))
    gate.add_argument("--binary", default=str(REPO / "target/release/kborg"))
    gate.add_argument("--only", action="append", choices=list(GATE_SCENARIOS))
    common(gate, 10)
    gate.set_defaults(func=cmd_gate, timeout=30)

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
