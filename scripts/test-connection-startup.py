#!/usr/bin/env python3
"""Offline PTY startup consent acceptance; stdlib, synthetic kubeconfig only."""
import argparse, datetime, fcntl, hashlib, json, os, pathlib, pty, re, runpy, select, socket, struct, subprocess, sys, tempfile, termios, threading, time

Screen = runpy.run_path(str(pathlib.Path(__file__).with_name('test-terminal.py')))['Screen']

parser=argparse.ArgumentParser()
parser.add_argument('--binary', required=True)
parser.add_argument('--output', required=True)
args=parser.parse_args()
results=[]
with tempfile.TemporaryDirectory(prefix='kborg-consent-') as scratch:
    root=pathlib.Path(scratch).resolve()
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
    def run(select_other, approve, changed=False, namespace=None, saved_view=False, selector=False):
        path.write_text(original)
        for name in ('prod','dev'): (root/(name+'.marker')).unlink(missing_ok=True)
        before=len(connections)
        master,slave=pty.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',32,100,0,0))
        env=os.environ.copy(); env.update(KUBECONFIG=str(path),KBORG_CONFIG_DIR=str(root/'settings'),KBORG_NO_SPLASH='1',TERM='xterm-256color',COLORTERM='truecolor',LANG='C.UTF-8')
        for key in list(env):
            if key.lower() in ('http_proxy','https_proxy','all_proxy'): env.pop(key)
        settings=root/'settings'/'contexts'
        settings.mkdir(parents=True,exist_ok=True)
        context_settings=settings/'company-prod.yaml'
        context_settings.unlink(missing_ok=True)
        if saved_view:
            context_settings.write_text('version: 1\nnamespace: payments\nview: deployments\nread_only: true\n')
        flags=['--namespace',namespace] if namespace else []
        if selector: flags.extend(['--selector','app=api'])
        process=subprocess.Popen([args.binary,'--theme','catppuccin-mocha','--color=always','--no-splash',*flags],stdin=slave,stdout=slave,stderr=slave,env=env,start_new_session=True)
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
        expected_view = 'Deployments' if saved_view else ('Pods' if selector else 'Nodes')
        expected_namespace = namespace or ('payments' if saved_view else ('team' if selector else 'all'))
        review_namespace = 'All namespaces' if expected_namespace == 'all' else expected_namespace
        def expect_review():
            wait_for('Connect to this kubeconfig context?')
            wait_for('Namespace: '+review_namespace)
            wait_for('Opens in: '+expected_view.lower() if saved_view else 'Opens in: '+expected_view)
        try:
            wait_for('company-prod'); expect_review()
            if saved_view: wait_for('Access: read only.')
            assert len(connections)==before
            assert not any(root.glob('*.marker'))
            if select_other:
                output.clear(); send('c'); wait_for('Choose an existing context')
                wait_for('> * company-prod'); wait_for('company-dev')
                picker=Screen(100,32); picker.feed(bytes(output))
                active, inactive=picker.cells[2:4]
                assert all(cell[2]==active[0][2] for cell in active)
                assert active[0][2]!=inactive[0][2], 'Selection must highlight the entire active row'
                output.clear(); send('\x1b'); expect_review()
                output.clear(); send('c'); wait_for('Choose an existing context')
                output.clear(); send('dev'); wait_for('company-dev')
                output.clear(); send('\r'); expect_review(); wait_for('company-dev')
                assert len(connections)==before and not any(root.glob('*.marker'))
            if changed:
                modified=json.loads(original); modified['clusters'][0]['cluster']['server']='https://127.0.0.1:9'
                path.write_text(json.dumps(modified)); output.clear(); send('y')
                wait_for('Kubeconfig changed after connection')
                wait_for('restart KBorg to review the updated context')
                assert len(connections)==before and not any(root.glob('*.marker'))
                send('q')
            elif approve:
                output.clear(); send('y'); marker=root/(('dev' if select_other else 'prod')+'.marker')
                wait_for('KBORG')
                wait_for(expected_view)
                end=time.monotonic()+10
                while time.monotonic()<end and not marker.exists():
                    if select.select([master],[],[],.1)[0]:
                        try: output.extend(os.read(master,65536))
                        except OSError: break
                assert marker.exists(), 'Configured helper must be reused after Yes'
                assert not (root/(('prod' if select_other else 'dev')+'.marker')).exists()
                screen=Screen(100,32); screen.feed(bytes(output))
                namespace_pattern=r'\bNamespace\s+'+re.escape(expected_namespace)+r'\b'
                end=time.monotonic()+5
                while time.monotonic()<end:
                    visible=screen.text()
                    if re.search(r'\bKBORG\s+'+expected_view+r'\b',visible) and re.search(namespace_pattern,visible):
                        break
                    if select.select([master],[],[],.1)[0]:
                        screen.feed(os.read(master,65536))
                else:
                    raise AssertionError(screen.text())
                send('q')
            else: send('\x1b')
            end=time.monotonic()+8
            while process.poll() is None and time.monotonic()<end:
                if select.select([master],[],[],.1)[0]:
                    try: output.extend(os.read(master,65536))
                    except OSError: break
            process.wait(timeout=1)
            assert process.returncode==0, process.returncode
            assert json.loads(path.read_text())['current-context']=='company-prod', 'Must not modify user kubeconfig/current-context'
            assert path.read_text()==(json.dumps(modified) if changed else original)
            if not approve: assert len(connections)==before and not any(root.glob('*.marker'))
            results.append({'alternative_context':select_other,'approved':approve,'changed_config_rejected':changed,'startup_view':expected_view,'namespace':expected_namespace,'helper_after_approval_only':True,'kubeconfig_unchanged':True,'clean_exit':True})
        finally:
            if process.poll() is None: process.terminate(); process.wait(timeout=5)
            os.close(master)
    try:
        run(False,False); run(True,False); run(False,True); run(True,True); run(False,False,True)
        run(False,True,namespace='cli-namespace',saved_view=True)
        run(False,True,saved_view=True)
        run(False,True,selector=True)
    finally: stopped.set(); listener.close(); thread.join(timeout=1)
report={'schema':1,'utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'binary_sha256':hashlib.sha256(pathlib.Path(args.binary).read_bytes()).hexdigest(),'scope':'PTY, synthetic kubeconfig and local listener; no real cluster or credentials','cases':results}
pathlib.Path(args.output).write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
