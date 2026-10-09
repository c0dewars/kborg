#!/usr/bin/env python3
"""Read-only 100-context alias and log-follow measurements; stdlib only.

Aliases intentionally refer to one existing server and identity. They do not
represent 100 independent control planes, credential providers or failure domains.
"""
import argparse
import copy
import hashlib
import json
import os
import pathlib
import select
import subprocess
import tempfile
import time


def measured(args, environment, limit):
    start = time.monotonic()
    process = subprocess.Popen(args, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    output = bytearray()
    errors = bytearray()
    peak = ticks = 0
    pipes = {process.stdout: output, process.stderr: errors}
    try:
        while process.poll() is None or pipes:
            if time.monotonic() - start > limit:
                raise RuntimeError('bounded measurement exceeded its deadline')
            try:
                for line in pathlib.Path(f'/proc/{process.pid}/status').read_text().splitlines():
                    if line.startswith(('VmHWM:', 'VmRSS:')):
                        peak = max(peak, int(line.split()[1]))
                fields = pathlib.Path(f'/proc/{process.pid}/stat').read_text().split(') ')[1].split()
                ticks = int(fields[11]) + int(fields[12])
            except FileNotFoundError:
                pass
            for pipe in select.select(list(pipes), [], [], .002)[0]:
                data = os.read(pipe.fileno(), 65536)
                if data:
                    pipes[pipe].extend(data)
                    if len(pipes[pipe]) > 2 * 1024 * 1024:
                        raise RuntimeError('measurement output exceeded limit')
                else:
                    del pipes[pipe]
        elapsed = time.monotonic() - start
        cpu = ticks / os.sysconf('SC_CLK_TCK')
        return {'seconds': round(elapsed, 6), 'peak_rss_kib': peak, 'cpu_ticks': ticks,
                'cpu_seconds': cpu, 'average_one_core_cpu_percent': round(cpu / elapsed * 100, 4),
                'exit_code': process.wait()}, bytes(output), bytes(errors)
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=5)
        process.stdout.close()
        process.stderr.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True, type=pathlib.Path)
    parser.add_argument('--fixture', required=True, type=pathlib.Path)
    parser.add_argument('--output', required=True, type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    fixture = args.fixture.resolve(strict=True)
    report = {'schema': 1, 'utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
              'platform': os.uname().sysname, 'kernel': os.uname().release,
              'logical_cpus': os.cpu_count(),
              'memory_total_kib': int(next(line for line in pathlib.Path('/proc/meminfo').read_text().splitlines()
                                           if line.startswith('MemTotal:')).split()[1]),
              'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'binary_bytes': binary.stat().st_size,
              'scope': 'Read-only existing Docker Desktop fixture; 100 aliases of ONE cluster/identity, not 100 clusters',
              'method': 'Linux procfs sampled at 2 ms; includes process startup; warm cache; one run per workload',
              'cpu_tick_seconds': 1 / os.sysconf('SC_CLK_TCK'),
              'limitation': 'A sampled RSS peak may miss short transients; zero ticks means below accounting resolution'}
    with tempfile.TemporaryDirectory(prefix='kborg-read-perf-') as temporary:
        directory = pathlib.Path(temporary)
        environment = os.environ.copy()
        environment['KBORG_CONFIG_DIR'] = str(directory / 'settings')
        environment['KBORG_NO_SPLASH'] = '1'
        config = json.loads(fixture.read_text(encoding='utf-8-sig'))
        original = copy.deepcopy(config['contexts'][0]['context'])
        aliases = [f'load-{index:03}' for index in range(100)]
        config['contexts'] = [{'name': name, 'context': copy.deepcopy(original)} for name in aliases]
        config['current-context'] = aliases[0]
        alias_file = directory / 'aliases.kubeconfig'
        # Parent is mode 0700; O_EXCL + mode0600 protects the credential copy from creation.
        fd = os.open(alias_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w') as stream:
            json.dump(config, stream)
        overview, data, errors = measured([str(binary), '--kubeconfig', str(alias_file), '--json',
                                           'overview', *aliases], environment, 120)
        rows = json.loads(data)
        assert overview['exit_code'] == 0 and not errors and len(rows) == 100
        assert {row['context'] for row in rows} == set(aliases)
        assert all(row['error'] is None and row['pods'] is not None and not row['truncated'] for row in rows)
        overview.update({'contexts': len(rows), 'distinct_clusters': 1, 'distinct_identities': 1,
                         'successful_rows': 100, 'maximum_configured_concurrency': 4,
                         'minimum_pod_count': min(row['pods'] for row in rows),
                         'maximum_pod_count': max(row['pods'] for row in rows)})
        report['overview_100_aliases'] = overview
        prefix = [str(binary), '--kubeconfig', str(fixture)]
        listed = subprocess.run([*prefix, '--json', 'list', 'pods'], env=environment,
                                capture_output=True, check=True, timeout=20)
        pod = json.loads(listed.stdout)['items'][0]
        pod_name = pod['metadata']['name']
        container = pod['spec']['containers'][0]['name']
        denied = subprocess.run([*prefix, 'logs', pod_name, container, '--follow'],
                                env=environment, capture_output=True, timeout=20)
        assert denied.returncode != 0 and not denied.stdout and b'opt in with --allow-logs' in denied.stderr
        report['log_opt_in'] = {'denied_without_flag': True, 'stdout_bytes': 0}
        logs, data, errors = measured([*prefix, '--allow-logs', '--json', 'logs', pod_name,
                                      container, '--follow'], environment, 75)
        lines = [json.loads(line)['line'] for line in data.splitlines()]
        assert lines and all(not any(ord(char) < 32 for char in line) for line in lines)
        assert b'60-second limit' in errors or b'timeout' in errors or b'transport failure' in errors
        assert logs['exit_code'] != 0
        logs.update({'lines': len(lines), 'stdout_bytes': len(data), 'container_selected': True,
                     'terminal_controls_absent': True, 'bounded_stop_reported': True,
                     'stop_reason': 'lifetime' if b'60-second limit' in errors else
                                    'idle/transport timeout', 'raw_logs_retained': False})
        report['live_log_follow'] = logs
    report['temporary_credential_copy_removed'] = True
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))


if __name__ == '__main__':
    main()
