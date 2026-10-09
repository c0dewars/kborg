#!/usr/bin/env python3
"""Linux measurements; stdlib only. Never interprets contexts as distinct clusters."""
import argparse
import concurrent.futures
import fcntl
import hashlib
import json
import os
import pathlib
import pty
import select
import statistics
import struct
import subprocess
import tempfile
import termios
import time


def measured(binary, arguments, directory):
    with tempfile.NamedTemporaryFile(dir=directory) as telemetry:
        started = time.monotonic()
        result = subprocess.run(
            ['/usr/bin/time', '-f', '%e %U %S %M', '-o', telemetry.name,
             str(binary), *arguments], stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, timeout=30, check=False)
        fields = pathlib.Path(telemetry.name).read_text().strip().splitlines()[-1].split()
        if result.returncode:
            raise RuntimeError('Benchmark command failed; refresh fixture credentials if applicable')
        return {'wall_ms': round((time.monotonic() - started) * 1000, 3),
                'cpu_ms': round((float(fields[1]) + float(fields[2])) * 1000, 3),
                'peak_rss_kib': int(fields[3]), 'output_bytes': len(result.stdout)}


def aggregate(samples):
    times = sorted(item['wall_ms'] for item in samples)
    return {'samples': len(samples), 'median_wall_ms': statistics.median(times),
            'p95_wall_ms': times[min(len(times) - 1, int(len(times) * .95))],
            'max_peak_rss_kib': max(item['peak_rss_kib'] for item in samples),
            'total_cpu_ms': sum(item['cpu_ms'] for item in samples)}


def terminal_measure(binary, duration, environment, arguments):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 35, 120, 0, 0))
    process = subprocess.Popen([str(binary), *arguments], stdin=slave, stdout=slave,
                               stderr=slave, env=environment, start_new_session=True)
    os.close(slave)
    started = time.monotonic()
    peak = 0
    cpu_ticks = 0
    output_bytes = 0
    try:
        while time.monotonic() - started < duration:
            if process.poll() is not None:
                raise RuntimeError('TUI exited before measurement completed')
            status = pathlib.Path(f'/proc/{process.pid}/status').read_text()
            for line in status.splitlines():
                if line.startswith(('VmHWM:', 'VmRSS:')):
                    peak = max(peak, int(line.split()[1]))
            stat = pathlib.Path(f'/proc/{process.pid}/stat').read_text().split(') ')[1].split()
            cpu_ticks = int(stat[11]) + int(stat[12])
            if select.select([master], [], [], .1)[0]:
                output_bytes += len(os.read(master, 65536))
        os.write(master, b'q')
        process.wait(timeout=5)
        if process.returncode:
            raise RuntimeError('TUI failed on clean exit')
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        os.close(master)
    cpu_seconds = cpu_ticks / os.sysconf('SC_CLK_TCK')
    return {'duration_seconds': duration, 'peak_rss_kib': peak,
            'cpu_seconds': round(cpu_seconds, 3),
            'average_one_core_cpu_percent': round(cpu_seconds / duration * 100, 3),
            'terminal_bytes': output_bytes,
            'includes_initial_connection_and_render': True}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True, type=pathlib.Path)
    parser.add_argument('--output', required=True, type=pathlib.Path)
    parser.add_argument('--fixture', type=pathlib.Path)
    parser.add_argument('--samples', type=int, default=30)
    parser.add_argument('--duration', type=int, default=30)
    options = parser.parse_args()
    if not 5 <= options.samples <= 100 or not 10 <= options.duration <= 120:
        parser.error('samples5..100 and duration10..120 required')
    binary = options.binary.resolve(strict=True)
    report = {'schema': 1, 'utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
              'platform': os.uname().sysname, 'kernel': os.uname().release,
              'binary_bytes': binary.stat().st_size,
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'method': 'GNU time per CLI process; procfs samples and PTY for TUI; warmed filesystem cache',
              'scope': '100 context aliases; one cluster if fixture supplied, zero otherwise; not100 clusters'}
    with tempfile.TemporaryDirectory(prefix='kborg-perf-') as temporary:
        directory = pathlib.Path(temporary)
        environment = os.environ.copy()
        environment['KBORG_CONFIG_DIR'] = str(directory / 'appearance')
        environment['KBORG_NO_SPLASH'] = '1'
        subprocess.run([str(binary), 'theme', 'use', 'catppuccin-mocha'],
                       env=environment, check=True, stdout=subprocess.DEVNULL)
        os.environ['KBORG_CONFIG_DIR'] = environment['KBORG_CONFIG_DIR']
        os.environ['KBORG_NO_SPLASH'] = '1'
        report['demo_cli'] = aggregate([measured(binary, ['--demo', '--json', 'list', 'pods'], directory)
                                        for _ in range(options.samples)])
        config = {'apiVersion': 'v1', 'kind': 'Config', 'current-context': 'context-000',
                  'clusters': [{'name': 'synthetic', 'cluster': {'server': 'https://127.0.0.1:9'}}],
                  'users': [{'name': 'synthetic', 'user': {'token': 'SYNTHETIC-NOT-A-CREDENTIAL'}}]}
        if options.fixture:
            config = json.loads(options.fixture.read_text())
        context = config.get('contexts', [{'context': {'cluster': 'synthetic', 'user': 'synthetic', 'namespace': 'default'}}])[0]['context']
        config['contexts'] = [{'name': f'context-{index:03d}', 'context': context.copy()} for index in range(100)]
        config['current-context'] = 'context-000'
        fixture = directory / 'hundred-contexts.kubeconfig'
        fixture.write_text(json.dumps(config))
        fixture.chmod(0o600)
        report['hundred_context_listing'] = aggregate([
            measured(binary, ['--kubeconfig', str(fixture), '--json', 'contexts'], directory)
            for _ in range(options.samples)])
        result = subprocess.run([str(binary), '--kubeconfig', str(fixture), '--json', 'contexts'],
                                check=True, stdout=subprocess.PIPE)
        if len(json.loads(result.stdout)) != 100:
            raise RuntimeError('Expected100 distinct context names')
        report['demo_tui'] = terminal_measure(binary, options.duration, environment, ['--demo'])
        if options.fixture:
            report['live_sequential_context_reads'] = aggregate([
                measured(binary, ['--kubeconfig', str(fixture), '--context', f'context-{index:03d}',
                                  '--json', 'list', 'pods'], directory) for index in range(100)])
            started = time.monotonic()
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
                samples = list(executor.map(lambda index: measured(binary,
                    ['--kubeconfig', str(fixture), '--context', f'context-{index:03d}', '--json', 'list', 'pods'], directory), range(100)))
            report['live_bounded_concurrency'] = aggregate(samples)
            report['live_bounded_concurrency']['workers'] = 8
            report['live_bounded_concurrency']['batch_wall_seconds'] = round(time.monotonic() - started, 3)
            report['live_tui'] = terminal_measure(binary, options.duration, environment,
                                                ['--kubeconfig', str(options.fixture)])
    options.output.parent.mkdir(parents=True, exist_ok=True)
    options.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
