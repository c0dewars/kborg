#!/usr/bin/env python3
"""Capture a small actual DEMO UI design iteration; never accesses a cluster."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import runpy
import select
import signal
import struct
import tempfile
import termios
import time

_helpers=runpy.run_path(str(Path(__file__).with_name("test-live-lifecycle-ui.py")))
Screen,_spawn=(_helpers[name] for name in ("Screen","_spawn"))

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,required=True)
    parser.add_argument("--screenshots",type=Path,required=True)
    args=parser.parse_args()
    binary=args.binary.resolve(strict=True)
    digest=hashlib.sha256(binary.read_bytes()).hexdigest()
    captures=[]
    with tempfile.TemporaryDirectory(prefix="kborg-ui-design-") as temporary:
        master,slave=pty.openpty()
        original=termios.tcgetattr(slave)
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",34,120,0,0))
        environment=os.environ.copy()
        environment.update(TERM="xterm-256color",COLORTERM="truecolor",KBORG_CONFIG_DIR=str(Path(temporary).resolve()))
        environment.pop("NO_COLOR",None)
        process=_spawn([str(binary),"--demo","--theme","catppuccin-mocha","--write","--allow-helm","--allow-helm-write","--allow-helm-hooks","--allow-helm-crds","--helm-tool","/demo/helm","--no-splash","--no-mouse","--color=always"],environment,slave)
        screen=Screen(120,34)
        def drain(seconds=.12):
            end=time.monotonic()+seconds
            while time.monotonic()<end:
                if select.select([master],[],[],min(.05,max(0,end-time.monotonic())))[0]:
                    try:data=os.read(master,65536)
                    except OSError:break
                    if not data:break
                    screen.feed(data.decode("utf-8",errors="replace"))
        def keys(value=b"",expected=None):
            if value:os.write(master,value)
            end=time.monotonic()+8
            while time.monotonic()<end:
                drain()
                if expected is None or expected in screen.text():return
                if process.poll() is not None:break
            raise RuntimeError(f"preview did not reach {expected!r}\n{screen.text()}")
        def capture(name):
            path=args.screenshots/f"{name}.png"
            screen.png(path)
            captures.append({"name":name,"path":str(path.resolve()),"width":screen.width,"height":screen.height})
        try:
            keys(expected="api-7d4f9")
            end=time.monotonic()+8
            while ("LOADING" in screen.text().splitlines()[2] or "Request in progress" in screen.text().splitlines()[-1]) and time.monotonic()<end:drain()
            capture("resources")
            keys(b":helm\r","HELM / releases")
            capture("helm-releases")
            keys(b"i","INSTALL HELM RELEASE")
            keys(b"\tdemo-new")
            capture("helm-install-form")
            keys(b"\r","HELM REVIEW")
            capture("helm-review-overview")
            keys(b"\x1b","INSTALL HELM RELEASE")
            keys(b"\x1b","HELM / releases")
            keys(b"t","TEST HELM RELEASE")
            capture("helm-test-form")
            keys(b"\x1b","HELM / releases")
            keys(b":helm-crds-delete\r","DELETE CHART CRDS")
            keys(b"\tdemo-schema")
            capture("crd-delete-form")
            fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",12,48,0,0))
            screen=Screen(48,12)
            os.killpg(process.pid,signal.SIGWINCH)
            drain(.4)
            capture("crd-delete-narrow")
            keys(b"\x1b")
            keys(b"\x1b")
            keys(b":q\r")
            end=time.monotonic()+6
            while process.poll() is None and time.monotonic()<end:drain()
            assert process.poll()==0,"preview child did not exit successfully"
            assert termios.tcgetattr(master)==original,"preview terminal mode not restored"
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=3)
            os.close(master)
            os.close(slave)
    assert hashlib.sha256(binary.read_bytes()).hexdigest()==digest,"binary changed during preview"
    report={"passed":True,"binary_sha256":digest,"terminal_restored":True,"screens":captures}
    args.screenshots.mkdir(parents=True,exist_ok=True)
    (args.screenshots/"report.json").write_text(json.dumps(report,indent=2)+"\n")
    print(json.dumps(report,indent=2))

if __name__=="__main__":main()
