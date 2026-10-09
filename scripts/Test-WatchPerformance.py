#!/usr/bin/env python3
"""Read-only WSL live/poll comparison and CLI smoke checks; stdlib only."""
import argparse, fcntl, hashlib, json, os, pathlib, pty, select, struct, subprocess, tempfile, termios, time


def tui(binary, fixture, environment, poll, duration):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 35, 120, 0, 0))
    args = [str(binary), '--kubeconfig', str(fixture)] + (['--poll'] if poll else [])
    process = subprocess.Popen(args, stdin=slave, stdout=slave, stderr=slave, env=environment, start_new_session=True)
    os.close(slave)
    started = time.monotonic(); peak = ticks = count = 0; output = bytearray()
    try:
        while time.monotonic() - started < duration:
            if process.poll() is not None: raise RuntimeError('TUI exited before measurement ended')
            for line in pathlib.Path(f'/proc/{process.pid}/status').read_text().splitlines():
                if line.startswith(('VmHWM:', 'VmRSS:')): peak = max(peak, int(line.split()[1]))
            stat = pathlib.Path(f'/proc/{process.pid}/stat').read_text().split(') ')[1].split()
            ticks = int(stat[11]) + int(stat[12])
            if select.select([master], [], [], .1)[0]:
                data = os.read(master, 65536); count += len(data); output.extend(data)
        os.write(master, b'q'); process.wait(timeout=5)
        if process.returncode: raise RuntimeError('TUI did not exit cleanly')
    finally:
        if process.poll() is None: process.terminate(); process.wait(timeout=5)
        os.close(master)
    expected = b'POLL' if poll else b'LIVE'
    if expected not in output: raise RuntimeError('Expected mode label absent')
    seconds = ticks / os.sysconf('SC_CLK_TCK')
    return {'duration_seconds': duration, 'peak_rss_kib': peak, 'cpu_ticks': ticks,
            'cpu_seconds': seconds, 'average_one_core_cpu_percent': round(seconds / duration * 100, 4),
            'terminal_bytes': count, 'expected_mode_label_seen': True, 'clean_exit': True,
            'includes_startup_and_connection': True, 'http_request_count': None}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True, type=pathlib.Path)
    parser.add_argument('--fixture', required=True, type=pathlib.Path)
    parser.add_argument('--output', required=True, type=pathlib.Path)
    parser.add_argument('--duration', type=int, default=30)
    parser.add_argument('--background-note', default='Not recorded by caller')
    options = parser.parse_args()
    if not 10 <= options.duration <= 120: parser.error('duration must be 10..120 seconds')
    binary = options.binary.resolve(strict=True); fixture = options.fixture.resolve(strict=True)
    report = {'schema': 1, 'utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
              'platform': os.uname().sysname, 'kernel': os.uname().release,
              'binary_bytes': binary.stat().st_size, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'cpu_tick_seconds': 1 / os.sysconf('SC_CLK_TCK'),
              'cpu_interpretation': 'Zero ticks means below kernel accounting resolution, not zero computation',
              'background_environment': options.background_note,
              'method': f'One sequential {options.duration} s run per mode; procfs sampled at 100 ms and PTY 120x35; warm filesystem cache',
              'scope': 'One existing Docker Desktop cluster, one small isolated read-only namespace; no cluster writes',
              'request_count_limitation': 'TLS network traffic does not prove HTTP request counts; no API audit or request instrumentation used'}
    with tempfile.TemporaryDirectory(prefix='kborg-watch-perf-') as directory:
        environment = os.environ.copy(); environment['KBORG_CONFIG_DIR'] = directory
        environment['KBORG_NO_SPLASH'] = '1'
        subprocess.run([str(binary), 'theme', 'use', 'catppuccin-mocha'], env=environment, check=True, stdout=subprocess.DEVNULL)
        prefix = [str(binary), '--kubeconfig', str(fixture), '--json']
        listed = subprocess.run([*prefix, 'list', 'pods'], env=environment, check=True, capture_output=True, timeout=20)
        list_count = len(json.loads(listed.stdout)['items'])
        watch = subprocess.Popen([*prefix, 'watch', 'pods'], env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        started = time.monotonic()
        try:
            if not select.select([watch.stdout], [], [], 20)[0]: raise RuntimeError('Watch first frame timed out')
            record = json.loads(watch.stdout.readline())
            assert not record['stale'] and not record['terminal'] and not record['truncated']
            assert len(record['items']) == list_count and record['resourceVersion']
            first_ms = round((time.monotonic() - started) * 1000, 3)
            watch.stdout.close(); closed = time.monotonic(); watch.wait(timeout=20)
            assert watch.returncode == 0
            report['live_cli_watch'] = {'first_snapshot_ms': first_ms, 'items': list_count,
                'valid_resource_version': True, 'initial_status': record['status'], 'broken_pipe_exit_code': 0,
                'broken_pipe_exit_seconds': round(time.monotonic() - closed, 3),
                'limitation': 'Snapshot and clean consumer-close tested; resource mutations intentionally delegated to separate acceptance suite'}
        finally:
            if watch.poll() is None: watch.terminate(); watch.wait(timeout=5)
        demo = subprocess.run([str(binary), '--demo', '--json', 'metrics', 'pods'], env=environment, check=True, capture_output=True, timeout=20)
        metrics = json.loads(demo.stdout)
        assert metrics['source'] == 'demo (deterministic sample)' and metrics['scope'] == 'pods' and metrics['items']
        assert all(row['cpu_nanocores'] >= 0 and row['memory_bytes'] >= 0 and row['timestamp'] and row['window'] for row in metrics['items'])
        report['demo_metrics'] = {'source': metrics['source'], 'items': len(metrics['items']), 'quantity_and_time_fields_present': True}
        live = subprocess.run([*prefix, 'metrics', 'pods'], env=environment, capture_output=True, timeout=20)
        assert live.returncode != 0 and not live.stdout and b'Metrics API read forbidden' in live.stderr
        report['desktop_metrics'] = {'exit_code': live.returncode, 'stdout_bytes': len(live.stdout), 'rbac_forbidden_message_seen': True}
        report['live_tui'] = tui(binary, fixture, environment, False, options.duration)
        report['poll_tui'] = tui(binary, fixture, environment, True, options.duration)
    options.output.parent.mkdir(parents=True, exist_ok=True)
    options.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))

if __name__ == '__main__': main()




