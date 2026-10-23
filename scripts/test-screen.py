#!/usr/bin/env python3
"""Regression checks for complete-frame PTY evidence, without a cluster."""
from pathlib import Path
import runpy
import unittest

Screen = runpy.run_path(str(Path(__file__).with_name("test-terminal.py")))["Screen"]


class CompleteFrameTest(unittest.TestCase):
    def test_incomplete_repaint_keeps_last_complete_scope_visible(self):
        screen = Screen(30, 3)
        screen.feed(b"\x1b[1;1HCTX desktop / READ ONLY")
        previous = screen.text()
        screen.feed(b"\x1b[?2026h\x1b[2J\x1b[2;1Hhealth updated")
        self.assertEqual(screen.text(), previous)
        self.assertEqual(screen.visible_cells()[0][0][0], "C")
        screen.feed(b"\x1b[1;1HCTX minikube / READ ONLY\x1b[?2026l")
        self.assertIn("CTX minikube", screen.text())
        self.assertIn("health updated", screen.text())

    def test_frame_markers_can_span_read_chunks(self):
        screen = Screen(10, 2)
        screen.feed(b"old\x1b[?20")
        screen.feed(b"26h\x1b[1;1Hnew\x1b[?2026")
        self.assertTrue(screen.text().startswith("old"))
        screen.feed(b"l")
        self.assertTrue(screen.text().startswith("new"))
        self.assertFalse(screen.synchronized)

    def test_unsynchronized_output_remains_visible(self):
        screen = Screen(10, 2)
        screen.feed(b"plain")
        self.assertTrue(screen.text().startswith("plain"))


if __name__ == "__main__":
    unittest.main()
