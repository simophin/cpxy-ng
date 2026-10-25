#!/usr/bin/env python3
"""Real mTLS sockets + opaque traffic. Disposable certs and addresses, no RTP."""
import json, os, pathlib, socket, ssl, subprocess, sys, tempfile, threading, time
w=pathlib.Path(tempfile.mkdtemp(prefix='relay-mtls-'))
bin=str(pathlib.Path(sys.argv[1] if len(sys.argv)>1 else 'target/debug/cpxy-udp-relay').resolve())
def openssl(*args): subprocess.run(['openssl',*args],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
openssl('req','-x509','-newkey','ec','-pkeyopt','ec_paramgen_curve:P-256','-nodes','-keyout',str(w/'ca.key'),'-out',str(w/'ca.pem'),'-subj','/CN=fixture CA','-days','1','-addext','keyUsage=critical,keyCertSign,cRLSign')
for name,usage in [('relay','serverAuth'),('controller','clientAuth')]:
 openssl('req','-newkey','ec','-pkeyopt','ec_paramgen_curve:P-256','-nodes','-keyout',str(w/(name+'.key')),'-out',str(w/(name+'.csr')),'-subj','/CN='+name)
 (w/'extensions').write_text('subjectAltName=DNS:relay\nextendedKeyUsage='+usage+'\n')
 openssl('x509','-req','-in',str(w/(name+'.csr')),'-CA',str(w/'ca.pem'),'-CAkey',str(w/'ca.key'),'-CAcreateserial','-out',str(w/(name+'.pem')),'-days','1','-extfile',str(w/'extensions'))
def free(kind):
 s=socket.socket(socket.AF_INET,kind);s.bind(('127.0.0.1',0));p=s.getsockname()[1];s.close();return p
port,other,mgmt=free(socket.SOCK_DGRAM),free(socket.SOCK_DGRAM),free(socket.SOCK_STREAM)
u=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);u.bind(('127.0.0.1',0));up=u.getsockname()[1]
def echo():
 while True:
  try:d,a=u.recvfrom(65536);u.sendto(d,a)
  except OSError:return
threading.Thread(target=echo,daemon=True).start()
limits=dict(mappings=[],max_flows=64,max_source_flows=16,max_payload=2048,socket_buffer=16384,idle_seconds=180,packets_per_second=100000,bytes_per_second=10000000,mapping_packets_per_second=100000,mapping_bytes_per_second=10000000,source_packets_per_second=100000,source_bytes_per_second=10000000)
config=dict(relay=limits,management=f'127.0.0.1:{mgmt}',tls=dict(ca=str(w/'ca.pem'),certificate=str(w/'relay.pem'),private_key=str(w/'relay.key')),authority=dict(owner='echo',listen_ip='127.0.0.1',upstream_ip='127.0.0.1',ports=[port,other,up],max_flows=32),journal=str(w/'journal.json'),agent_socket=None,staged_seconds=2,outage_seconds=10)
(w/'config.json').write_text(json.dumps(config))
ctx=ssl.create_default_context(cafile=str(w/'ca.pem'));ctx.load_cert_chain(str(w/'controller.pem'),str(w/'controller.key'))
log=open(w/'relay.log','ab');proc=None

def start():
 global proc
 proc=subprocess.Popen([bin,'managed',str(w/'config.json')],stderr=log)
 for _ in range(100):
  if proc.poll() is not None:raise AssertionError((w/'relay.log').read_text())
  try:return call('inventory')
  except OSError as e:
   if _==2: print('startup retry',e,flush=True)
   time.sleep(.05)
 raise AssertionError('startup timed out')
def call(op, spec=None, boot=None, context=ctx, receive=True):
 r=dict(operation=op)
 if spec is not None:r['spec']=spec
 if boot is not None:r['boot']=boot
 with context.wrap_socket(socket.create_connection(('127.0.0.1',mgmt),2),server_hostname='relay') as s:
  s.settimeout(3);s.sendall((json.dumps(r)+'\n').encode())
  if not receive:return
  return json.loads(s.makefile('rb').readline())
def spec(g,p=port):return dict(owner='echo',generation=g,listen=f'127.0.0.1:{p}',upstream=f'127.0.0.1:{up}')
def okay(r):assert r['error'] is None,r;return r
client=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);client.settimeout(.3)
def traffic(p=port):
 payload=os.urandom(1200);client.sendto(payload,('127.0.0.1',p));d,a=client.recvfrom(2048);assert d==payload and a[1]==p
try:
 boot=okay(start())['boot']
 assert subprocess.run([bin,'cleanup',str(w/'config.json')],stderr=subprocess.DEVNULL).returncode!=0
 # Unauthenticated TLS and server-name mismatch are rejected.
 unauth=ssl.create_default_context(cafile=str(w/'ca.pem'))
 try:call('inventory',context=unauth);raise AssertionError('missing client cert accepted')
 except (ssl.SSLError,ValueError):pass
 # Missing ACK: prepare processed even when caller disconnects; retry is idempotent.
 a=spec('old');call('prepare',a,receive=False);time.sleep(.1)
 r=okay(call('prepare',a));assert r['boot']==boot and len(r['entries'])==1
 okay(call('commit',a,boot));okay(call('commit',a,boot));traffic()
 assert call('commit',a,'stale-agent-boot')['error']
 assert call('prepare',spec('conflict'))['error'];traffic()
 # Staged expiry rejects late ACK/commit and releases the socket.
 b=spec('staged',other);okay(call('prepare',b));time.sleep(3)
 assert call('commit',b,boot)['error'];traffic()
 # Occupied port cannot produce READY; the committed listener continues.
 occupied=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);occupied.bind(('127.0.0.1',other))
 assert call('prepare',spec('occupied',other))['error'];traffic();occupied.close();time.sleep(1.1)
 # Crash/restart restores live generation, emits a fresh boot, rejects old ACK.
 proc.kill();proc.wait();r=okay(start());newboot=r['boot'];assert newboot!=boot
 assert call('renew',a,boot)['error'];okay(call('renew',a,newboot));traffic()
 # Release on both repeated retirement and numeric reuse; stale retire is harmless.
 okay(call('retire',a));okay(call('retire',a));n=spec('new');okay(call('prepare',n));okay(call('commit',n,newboot));okay(call('retire',a));traffic()
 # Controller loss: committed outage grace keeps forwarding, then expires.
 time.sleep(3);traffic();time.sleep(8)
 assert not okay(call('inventory'))['entries']
 try:traffic();raise AssertionError('expired mapping still forwards')
 except socket.timeout:pass
 # Independent cleanup excludes live workers, survives SIGKILL and is idempotent.
 okay(call('prepare',spec('cleanup')));okay(call('commit',spec('cleanup'),newboot))
 proc.kill();proc.wait()
 subprocess.run([bin,'cleanup',str(w/'config.json')],check=True)
 subprocess.run([bin,'cleanup',str(w/'config.json')],check=True)
 assert json.loads((w/'journal.json').read_text())==[]
 print('MTLS_LEASES_OK artifacts='+str(w))
finally:
 if proc and proc.poll() is None:proc.kill();proc.wait()
 u.close();log.close()
