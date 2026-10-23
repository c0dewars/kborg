#!/usr/bin/env python3
"""Actual terminal captures and local export acceptance using offline demo data."""
import argparse, fcntl, hashlib, json, os, pty, runpy, select, signal, struct, tempfile, termios, time
from pathlib import Path
helpers=runpy.run_path(str(Path(__file__).with_name('test-live-lifecycle-ui.py')))
Screen,spawn=helpers['Screen'],helpers['_spawn']
def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary',type=Path,required=True);p.add_argument('--screenshots',type=Path,required=True)
    a=p.parse_args();binary=a.binary.resolve();digest=hashlib.sha256(binary.read_bytes()).hexdigest();captures=[];checks=[]
    with tempfile.TemporaryDirectory(prefix='kborg-v1-exports-') as temporary:
        config=Path(temporary).resolve();master,slave=pty.openpty();original=termios.tcgetattr(slave)
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',34,120,0,0));screen=Screen(120,34)
        env=os.environ.copy();env.update(TERM='xterm-256color',COLORTERM='truecolor',LC_ALL='en_US.UTF-8',KBORG_CONFIG_DIR=str(config));env.pop('NO_COLOR',None)
        proc=spawn([str(binary),'--demo','--allow-logs','--allow-helm','--theme','rose-pine','--no-splash','--no-mouse','--color=always'],env,slave)
        def drain(seconds=.15):
            end=time.monotonic()+seconds
            while time.monotonic()<end:
                if select.select([master],[],[],min(.05,max(0,end-time.monotonic())))[0]:
                    try:data=os.read(master,65536)
                    except OSError:break
                    if not data:break
                    screen.feed(data)
        def keys(value=b'',expected=None):
            if value:os.write(master,value)
            end=time.monotonic()+10
            while time.monotonic()<end:
                drain()
                if expected is None or expected in screen.text():drain(.2);return
            raise RuntimeError(f'missing {expected}:\n{screen.text()}')
        def capture(name):
            path=a.screenshots/f'{name}.png';screen.png(path);captures.append({'name':name,'path':str(path.resolve())})
        try:
            keys(expected='api-7d4f9');drain(.5)
            keys(b':export\r','EXPORT / review local file');capture('resource-export-review')
            keys(b'\x1b');assert not (config/'exports').exists();checks.append('resource export cancel writes no file')
            keys(b':export\r','EXPORT / review local file');keys(b'\r','EXPORT RESULT');keys(expected='Saved');capture('resource-export-saved');keys(b'\x1b')
            files=list((config/'exports').glob('*.yaml'));assert len(files)==1;assert 'api-7d4f9' in files[0].read_text();assert files[0].stat().st_mode & 0o777==0o600
            checks.append('resource YAML explicit save and private permissions')
            keys(b'l','Logs');drain(.5);keys(b'e');capture('log-error-filter');keys(b'e');keys(b'\x13','EXPORT / review local file');capture('log-export-review');keys(b'\r','EXPORT RESULT');keys(expected='Saved');keys(b'\x1b','Logs')
            files=list((config/'exports').glob('*.log'));assert len(files)==1;content=files[0].read_text();assert '# Source:' in content and 'api-7d4f9' in content and '# KBorg captured log window' in content;checks.append('log export contains source identity and retained window')
            keys(b'\x1b');keys(b':helm\r','HELM / releases');keys(b'\x13','EXPORT / review local file');capture('helm-export-review');keys(b'\r','EXPORT RESULT');keys(expected='Saved')
            files=list((config/'exports').glob('*.json'));assert len(files)==1;value=json.loads(files[0].read_text());assert 'release' in value and 'resources' in value and 'values' not in value;checks.append('Helm export selected metadata only')
            keys(b'\x1b');keys(b':q\r');end=time.monotonic()+5
            while proc.poll() is None and time.monotonic()<end:drain()
            assert proc.poll()==0;assert termios.tcgetattr(master)==original;checks.append('terminal restored')
        finally:
            if proc.poll() is None:proc.kill();proc.wait(timeout=5)
            os.close(master);os.close(slave)
    assert hashlib.sha256(binary.read_bytes()).hexdigest()==digest
    report={'passed':True,'binary_sha256':digest,'scope':'Actual offline demo terminal and explicit local exports; no cluster writes','checks':checks,'screens':captures,'temporary_files_removed':True}
    a.screenshots.mkdir(parents=True,exist_ok=True);(a.screenshots/'report.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
if __name__=='__main__':main()
