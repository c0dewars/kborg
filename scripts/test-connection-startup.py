#!/usr/bin/env python3
"""Offline PTY startup consent acceptance; stdlib, synthetic kubeconfig only."""
import argparse, datetime, fcntl, hashlib, json, os, pathlib, pty, select, socket, struct, subprocess, sys, tempfile, termios, threading, time

parser=argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--output', required=True)
args=parser.parse_args()
results=[]
with tempfile.TemporaryDirectory(prefix='kborg-consent-') as scratch:
    root=pathlib.Path(scratch)
    listener=socket.socket(); listener.bind(('127.0.0.1',0)); listener.listen(); listener.settimeout(.1)
    connections=[]; stopped=threading.Event()
    def accept():
        while not stopped.is_set():
            try:
                client,_=listener.accept(); connections.append(1); client.close()
            except socket.timeout: pass
            except OSError: return
    thread=threading.Thread(target=accept,daemon=True); thread.start()
    helper=root/'helper.py'
    helper.write_text('import json,pathlib,sys\npathlib.Path(sys.argv[1]).write_text("called")\nprint(json.dumps({"apiVersion":"client.authentication.k8s.io/v1","kind":"ExecCredential","status":{"token":"synthetic-test-only"}}))\n')
    config={"apiVersion":"v1","kind":"Config","current-context":"company-prod","clusters":[{"name":"company","cluster":{"server":f"https://127.0.0.1:{listener.getsockname()[1]}","insecure-skip-tls-verify":True}}],"contexts":[],"users":[]}
    for name in ('prod','dev'):
        config['contexts'].append({'name':'company-'+name,'context':{'cluster':'company','user':name,'namespace':'team'}})
        config['users'].append({'name':name,'user':{'exec':{'apiVersion':'client.authentication.k8s.io/v1','command':sys.executable,'args':[str(helper),str(root/(name+'.marker'))],'interactiveMode':'Never'}}})
    path=root/'company-kubeconfig.json'; original=json.dumps(config); path.write_text(original)
    def run(select_other, approve, changed=False):
        path.write_text(original)
        for name in ('prod','dev'): (root/(name+'.marker')).unlink(missing_ok=True)
        before=len(connections)
        master,slave=pty.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',32,100,0,0))
        env=os.environ.copy(); env.update(KUBECONFIG=str(path),KBORG_CONFIG_DIR=str(root/'settings'),KBORG_NO_SPLASH='1',TERM='xterm-256color',LANG='C.UTF-8')
        for key in list(env):
            if key.lower() in ('http_proxy','https_proxy','all_proxy'): env.pop(key)
        process=subprocess.Popen([args.binary,'--theme','catppuccin-mocha','--no-splash'],stdin=slave,stdout=slave,stderr=slave,env=env,start_new_session=True)
        os.close(slave); output=bytearray()
        def wait_for(text,timeout=10):
            end=time.monotonic()+timeout
            while time.monotonic()<end:
                if text.encode() in output: return
                if select.select([master],[],[],.1)[0]:
                    try: output.extend(os.read(master,65536))
                    except OSError: break
                if process.poll() is not None: break
            raise AssertionError('Expected screen text not found: '+text+'; tail='+repr(bytes(output[-300:])))
        def send(text): os.write(master,text.encode())
        try:
            wait_for('company-prod'); wait_for('Connect to this kubeconfig context?')
            assert len(connections)==before
            assert not any(root.glob('*.marker'))
            if select_other:
                output.clear(); send('n'); wait_for('Choose an existing context')
                output.clear(); send('dev'); wait_for('company-dev')
                output.clear(); send('\r'); wait_for('Connect to this kubeconfig context?'); wait_for('company-dev')
                assert len(connections)==before and not any(root.glob('*.marker'))
            if changed:
                modified=json.loads(original); modified['clusters'][0]['cluster']['server']='https://127.0.0.1:9'
                path.write_text(json.dumps(modified)); output.clear(); send('y')
                wait_for('Kubeconfig changed after connection')
                wait_for('restart KBorg to review the updated context')
                assert len(connections)==before and not any(root.glob('*.marker'))
                send('q')
            elif approve:
                send('y'); marker=root/(('dev' if select_other else 'prod')+'.marker')
                end=time.monotonic()+10
                while time.monotonic()<end and not marker.exists():
                    if select.select([master],[],[],.1)[0]:
                        try: output.extend(os.read(master,65536))
                        except OSError: break
                assert marker.exists(), 'Configured helper must be reused after Yes'
                assert not (root/(('prod' if select_other else 'dev')+'.marker')).exists()
                send('q')
            else: send('\x1b')
            process.wait(timeout=8)
            assert process.returncode==0, process.returncode
            assert json.loads(path.read_text())['current-context']=='company-prod', 'Must not modify user kubeconfig/current-context'
            assert path.read_text()==(json.dumps(modified) if changed else original)
            if not approve: assert len(connections)==before and not any(root.glob('*.marker'))
            results.append({'alternative_context':select_other,'approved':approve,'changed_config_rejected':changed,'helper_after_approval_only':True,'kubeconfig_unchanged':True,'clean_exit':True})
        finally:
            if process.poll() is None: process.terminate(); process.wait(timeout=5)
            os.close(master)
    try:
        run(False,False); run(True,False); run(False,True); run(True,True); run(False,False,True)
    finally: stopped.set(); listener.close(); thread.join(timeout=1)
report={'schema':1,'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'binary_sha256':hashlib.sha256(pathlib.Path(args.binary).read_bytes()).hexdigest(),'scope':'WSL PTY, synthetic kubeconfig and local listener; no real cluster or credentials','cases':results}
pathlib.Path(args.output).write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
