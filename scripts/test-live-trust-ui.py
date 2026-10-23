#!/usr/bin/env python3
"""Capture an actual owned-cluster migration review and cancel without submitting it."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import runpy
import select
import signal
import struct
import termios
import time

_helpers = runpy.run_path(str(Path(__file__).with_name('test-live-lifecycle-ui.py')))
Screen, spawn = (_helpers[name] for name in ('Screen', '_spawn'))


def run(binary, environment, argv, namespace, release, chart, screenshots):
    assert namespace.startswith('kborg-migration-'), 'UI harness requires an owned migration namespace'
    binary = Path(binary).resolve(strict=True)
    screenshots = Path(screenshots)
    binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
    master, slave = pty.openpty()
    original = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 34, 120, 0, 0))
    environment = environment.copy()
    environment.update(TERM='xterm-256color', COLORTERM='truecolor')
    environment.pop('NO_COLOR', None)
    process = spawn([str(binary), *map(str, argv), '--theme', 'rose-pine', '--no-splash', '--no-mouse', '--color=always'], environment, slave)
    screen = Screen(120, 34)
    captures = []
    total = 0
    deadline = time.monotonic() + 160

    def drain(seconds=.15):
        nonlocal total
        until = min(time.monotonic() + seconds, deadline)
        while time.monotonic() < until:
            if select.select([master], [], [], min(.05, max(0, until-time.monotonic())))[0]:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                total += len(data)
                assert total <= 16*1024*1024, 'UI output exceeded bound'
                screen.feed(data)
        assert time.monotonic() < deadline, 'live UI deadline exceeded'

    def keys(value=b'', expected=None):
        if value:
            os.write(master, value)
        until = min(deadline, time.monotonic()+125)
        while time.monotonic() < until:
            drain()
            if expected is None or expected in screen.text():
                drain(.15)
                return
            assert process.poll() is None, 'live UI exited before requested state'
        raise AssertionError('live UI did not reach expected state: '+str(expected))

    def capture(name):
        path = screenshots / (name+'.png')
        screen.png(path)
        captures.append({'name': name, 'path': str(path.resolve()), 'width':120, 'height':34})

    try:
        keys(expected='CTX ')
        for _ in range(100):
            if 'LOADING' not in screen.text().splitlines()[2] and 'Request in progress' not in screen.text().splitlines()[-1]:
                break
            drain(.1)
        keys(b':helm-crds-migrate\r', 'MIGRATE CRD STORAGE')
        keys(b'\t'+release.encode()+b'\t\x15'+str(chart).encode(), Path(chart).name)
        capture('migration-form')
        keys(b'\r', 'HELM REVIEW')
        capture('migration-review-overview')
        for page in range(120):
            if re.search(r'\b0 unread\b', screen.text().splitlines()[-1]):
                break
            keys(b'\x1b[6~')
            if page in (0, 2, 5):
                capture('migration-review-page-'+str(page+2))
        else:
            raise AssertionError('complete live migration review could not be painted')
        assert re.search(r'\b0 unread\b', screen.text().splitlines()[-1]), 'review still has unread lines'
        assert 'MIGRATE CRDS '+namespace+'/'+release in screen.text(), 'strong migration confirmation missing'
        capture('migration-review-ready')
        keys(b'\x1b', 'MIGRATE CRD STORAGE')
        capture('migration-review-cancelled')
        keys(b'\x1b')
        keys(b':q\r')
        until = time.monotonic()+8
        while process.poll() is None and time.monotonic() < until:
            drain()
        assert process.poll() == 0, 'live UI failed to exit cleanly'
        assert termios.tcgetattr(master) == original, 'terminal mode was not restored'
    except (AssertionError, OSError):
        # Only this namespace's fixed public fixtures are visible to the reader.
        # Keep a bounded rendered frame for startup/form diagnostics, never a
        # terminal transcript or the private kubeconfig/credential environment.
        screenshots.mkdir(parents=True, exist_ok=True)
        capture('migration-failure-state')
        raise
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=3)
        os.close(master)
        os.close(slave)
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == binary_hash, 'binary changed during UI review'
    report = {'passed': True, 'binary_sha256':binary_hash, 'terminal_restored':True,
              'review_cancelled_without_approval':True, 'screens':captures}
    screenshots.mkdir(parents=True, exist_ok=True)
    (screenshots/'report.json').write_text(json.dumps(report,indent=2)+'\n')
    return report
