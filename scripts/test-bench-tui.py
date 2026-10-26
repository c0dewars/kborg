#!/usr/bin/env python3
"""Regression tests for the measurement harness, using only a local PTY."""
import contextlib
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("bench_tui", Path(__file__).with_name("bench-tui.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class BenchmarkHarnessTests(unittest.TestCase):
    def test_navigation_latency_does_not_shorten_idle_sampling(self):
        fixture = "import time; print('READY', flush=True); input(); print('NEXT', flush=True); time.sleep(60)"
        result = bench.measure("fixture", [sys.executable, "-u", "-c", fixture], "READY",
                               steps=[("\\n", "NEXT")], seconds=2, timeout=5)
        self.assertEqual(result["idle_seconds"], 2)
        self.assertGreaterEqual(result["sampled_seconds"], 2)
        self.assertGreaterEqual(result["sample_count"], 2)
        self.assertLess(result["steps"][0]["seconds"], 2)

    def test_missing_scale_marker_fails_the_gate(self):
        args = SimpleNamespace(budgets=str(Path(__file__).with_name("bench-budgets.json")),
                               binary=sys.executable, only=["demo-scale"], repeat=1,
                               seconds=1, timeout=1, out=None)
        with mock.patch.object(bench, "measure_repeated", side_effect=bench.MarkerMissing("scale", "absent")), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                bench.cmd_gate(args)
        self.assertEqual(raised.exception.code, 1)


if __name__ == "__main__":
    unittest.main()
