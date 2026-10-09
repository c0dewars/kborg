import sys,os,json,time,subprocess,pathlib,tempfile,pty,fcntl,termios,struct,select,statistics,hashlib,re
binary=pathlib.Path.home()/'.cache/kborg-target/release/kborg'
fixture=pathlib.Path(sys.argv[1])
report={'schema':1,'utc':time.strftime('%Y-%m-%dT%H:%M:%SZ',time.gmtime()),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'scope':'One Docker Desktop Kubernetes 1.36.1 cluster, 1001 owned synthetic ConfigMaps with 4096-byte data each; no application workloads','method':'GNU time CLI peak RSS; procfs TUI RSS/CPU; PTY output acknowledgement latency; first process cool, subsequent warm, no cache eviction'}
with tempfile.TemporaryDirectory(prefix='kborg-load-') as d:
 env=os.environ.copy();env['KBORG_CONFIG_DIR']=d;env['KBORG_NO_SPLASH']='1'
 args=[str(binary),'--kubeconfig',str(fixture),'--theme','catppuccin-mocha','--no-splash']
 samples=[]
 for i in range(6):
  t=pathlib.Path(d)/'time';start=time.monotonic()
  r=subprocess.run(['/usr/bin/time','-f','%e %U %S %M','-o',str(t),*args,'--json','list','configmaps'],env=env,capture_output=True,timeout=25)
  if r.returncode: raise RuntimeError(r.stderr.decode()[:500])
  fields=t.read_text().split();v=json.loads(r.stdout)
  assert len(v['items'])==1000 and v['truncated'] is True
  assert b'KBORG_LOAD_DATA_CANARY' not in r.stdout and all(x.get('data') == '[REDACTED]' for x in v['items'])
  samples.append({'wall_ms':round((time.monotonic()-start)*1000,2),'cpu_ms':round((float(fields[1])+float(fields[2]))*1000,2),'peak_rss_kib':int(fields[3]),'output_bytes':len(r.stdout),'count':len(v['items']),'truncated':v['truncated']})
 report['cli']={'cool':samples[0],'warm':samples[1:],'redaction_canary_absent':True,'positive_truncation_bound':True}
 r=subprocess.run([*args,'list','configmaps'],env=env,capture_output=True,timeout=25)
 assert r.returncode==0 and b'TRUNCATED' in r.stderr
 report['cli']['text_truncation_warning']=True
 m,s=pty.openpty();fcntl.ioctl(s,termios.TIOCSWINSZ,struct.pack('HHHH',35,160,0,0))
 p=subprocess.Popen(args,stdin=s,stdout=s,stderr=s,env=env,start_new_session=True);os.close(s)
 def read_until(needle,timeout=20):
  start=time.monotonic();buf=b''
  while time.monotonic()-start<timeout:
   if p.poll() is not None: raise RuntimeError('TUI exited unexpectedly')
   if select.select([m],[],[],.05)[0]:
    buf+=os.read(m,262144)
    if needle in re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]',b'',buf):return round((time.monotonic()-start)*1000,2)
  raise RuntimeError('TUI output readiness timeout: '+str(needle))
 try:
  time.sleep(1);os.write(m,b'\t'+b'j'*10+b'\r');ready=read_until(b'1000 objects')
  def usage():
   status=pathlib.Path(f'/proc/{p.pid}/status').read_text();stat=pathlib.Path(f'/proc/{p.pid}/stat').read_text().split(') ')[1].split()
   return int(re.search(r'VmRSS:\s+(\d+)',status)[1]),int(stat[11])+int(stat[12])
  rss0,cpu0=usage();peak=rss0;start=time.monotonic();idle_bytes=0
  while time.monotonic()-start<15:
   peak=max(peak,usage()[0])
   if select.select([m],[],[],.1)[0]:idle_bytes+=len(os.read(m,262144))
  rss1,cpu1=usage();elapsed=time.monotonic()-start
  report['tui_idle']={'loaded_objects':1000,'ready_ms_after_navigation':ready,'duration_seconds':round(elapsed,2),'peak_sampled_rss_kib':peak,'start_rss_kib':rss0,'end_rss_kib':rss1,'cpu_seconds':(cpu1-cpu0)/os.sysconf('SC_CLK_TCK'),'includes_periodic_5_second_api_refresh':True,'terminal_bytes':idle_bytes}
  latency=[]
  for _ in range(5):
   os.write(m,b'/load-0999\r');latency.append(read_until(b'filter: load-0999',3));os.write(m,b'\x1b');read_until(b'filter: ',3)
  report['cached_filter']={'input':'load-0999','samples_ms':latency,'median_ms':statistics.median(latency),'max_ms':max(latency),'method':'PTY bytes written through output of filter indicator, no screen presentation latency or server audit verification'}
  os.write(m,b'q');p.wait(timeout=5);assert p.returncode==0
 finally:
  if p.poll() is None:p.terminate();p.wait(timeout=5)
  os.close(m)
out=pathlib.Path(sys.argv[2]);out.parent.mkdir(exist_ok=True);out.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2))


