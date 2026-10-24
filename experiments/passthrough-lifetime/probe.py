"""Actual passthrough mapping lifetime; no production sealing or performance claim."""
import argparse,ctypes,errno,fcntl,json,mmap,os,platform,select,signal,subprocess,tempfile,time
from pathlib import Path
libc=ctypes.CDLL(None,use_errno=True)
libc.mmap.argtypes=[ctypes.c_void_p,ctypes.c_size_t,ctypes.c_int,ctypes.c_int,ctypes.c_int,ctypes.c_long];libc.mmap.restype=ctypes.c_void_p
libc.munmap.argtypes=[ctypes.c_void_p,ctypes.c_size_t];libc.munmap.restype=ctypes.c_int
libc.msync.argtypes=[ctypes.c_void_p,ctypes.c_size_t,ctypes.c_int];libc.msync.restype=ctypes.c_int

def lease(path):
 fd=os.open(path,os.O_RDWR)
 try:
  try:fcntl.fcntl(fd,fcntl.F_SETLEASE,fcntl.F_WRLCK)
  except OSError as e:
   assert e.errno==errno.EAGAIN,e
   return False
  fcntl.fcntl(fd,fcntl.F_SETLEASE,fcntl.F_UNLCK);return True
 finally:os.close(fd)

def wait(test,timeout=3):
 deadline=time.monotonic()+timeout
 while not test():
  if time.monotonic()>=deadline:return False
  time.sleep(.01)
 return True

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,required=True)
parser.add_argument('--work-root',type=Path,required=True)
args=parser.parse_args()
with tempfile.TemporaryDirectory(prefix='sl7-passthrough-proof-',dir=args.work_root) as directory:
 root=Path(directory);native=root/'data';native.write_bytes(b'x'*4096);mount=root/'mount';mount.mkdir();events=root/'events';events.touch()
 def count(event):return events.read_text().splitlines().count(event)
 rows=[]
 with (root/'daemon.log').open('w') as log:
  daemon=subprocess.Popen([str(args.binary.resolve()),str(native),str(mount),str(events)],stdout=log,stderr=log)
  try:
   assert wait(lambda:os.path.ismount(mount) or daemon.poll() is not None), 'mount timeout'
   assert daemon.poll() is None,(root/'daemon.log').read_text()
   for name,flags,prot,mapflags in [('readonly',os.O_RDONLY,mmap.PROT_READ,mmap.MAP_SHARED),('private-writable',os.O_RDONLY,mmap.PROT_READ|mmap.PROT_WRITE,mmap.MAP_PRIVATE),('shared-writable',os.O_RDWR,mmap.PROT_READ|mmap.PROT_WRITE,mmap.MAP_SHARED)]:
    native.write_bytes(b'x'*4096);before=count('release');fd=os.open(mount/'data',flags)
    try:pointer=libc.mmap(None,4096,prot,mapflags,fd,0)
    finally:os.close(fd)
    assert pointer!=ctypes.c_void_p(-1).value,ctypes.get_errno()
    try:
     early=wait(lambda:count('release')>before,timeout=.5)
     assert ctypes.string_at(pointer,1)==b'x'
     assert not lease(native),f'{name}: live mapping did not block retirement lease'
     if name!='readonly':ctypes.memmove(pointer,b'y',1)
     if name=='shared-writable':assert libc.msync(pointer,4096,4)==0
     expected=b'y' if name=='shared-writable' else b'x'
     # Re-register/open the same inode while the earlier native mapping survives.
     reopened=os.open(mount/'data',os.O_RDONLY)
     try:assert os.read(reopened,1)==expected
     finally:os.close(reopened)
     assert native.read_bytes()[:1]==expected
     assert ctypes.string_at(pointer,1)==(b'x' if name=='readonly' else b'y')
     assert not lease(native)
    finally:assert libc.munmap(pointer,4096)==0
    assert wait(lambda:count('release')>=before+2),events.read_text()
    assert wait(lambda:lease(native)),f'{name}: lease not granted after munmap'
    rows.append(dict(mapping=name,release_before_munmap=early,lease_blocked_with_mapping=True,lease_granted_after_munmap=True,reopen_while_mapped=True,private_or_shared_write_visibility='passed'))
   # The only surviving reference now belongs to a different process's VMA.
   native.write_bytes(b'x'*4096);before=count('release');fd=os.open(mount/'data',os.O_RDWR)
   try:pointer=libc.mmap(None,4096,mmap.PROT_READ|mmap.PROT_WRITE,mmap.MAP_SHARED,fd,0)
   finally:os.close(fd)
   assert pointer!=ctypes.c_void_p(-1).value
   command_read,command_write=os.pipe();ack_read,ack_write=os.pipe()
   child=os.fork()
   if child==0:
    try:
     os.close(command_write);os.close(ack_read)
     assert os.read(command_read,1)==b'w'
     ctypes.memmove(pointer,b'child',5);assert libc.msync(pointer,4096,4)==0
     os.write(ack_write,b'w');assert os.read(command_read,1)==b'z'
     assert libc.munmap(pointer,4096)==0;os.write(ack_write,b'z')
    except BaseException:os._exit(1)
    os._exit(0)
   os.close(command_read);os.close(ack_write)
   reaped=False
   def ack(expected):
    assert select.select([ack_read],[],[],3)[0], 'child response timeout'
    assert os.read(ack_read,1)==expected
   try:
    early=wait(lambda:count('release')>before,timeout=.5)
    assert libc.munmap(pointer,4096)==0;pointer=None
    assert not lease(native),'child mapping must block retirement after parent munmap'
    os.write(command_write,b'w');ack(b'w');assert native.read_bytes()[:5]==b'child'
    with (mount/'data').open('rb') as reopened:assert reopened.read(5)==b'child'
    assert not lease(native)
    os.write(command_write,b'z');ack(b'z')
    waited,status=os.waitpid(child,0);reaped=True;assert waited==child and status==0
    assert wait(lambda:count('release')>=before+2)
    assert wait(lambda:lease(native))
    rows.append(dict(mapping='forked-shared-writable',release_before_munmap=early,lease_blocked_with_mapping=True,lease_granted_after_munmap=True,reopen_while_mapped=True,private_or_shared_write_visibility='passed',parent_unmapped_before_child_write=True))
   finally:
    if pointer is not None:libc.munmap(pointer,4096)
    if not reaped:
     try:os.kill(child,signal.SIGKILL)
     except ProcessLookupError:pass
     os.waitpid(child,0)
    os.close(command_write);os.close(ack_read)
  finally:
   if os.path.ismount(mount):subprocess.check_call(['fusermount3','-u',str(mount)])
   daemon.wait(timeout=10)
  assert daemon.returncode==0,(root/'daemon.log').read_text()
 result=dict(status='passed',kernel=platform.release(),filesystem=subprocess.check_output(['findmnt','-n','-o','FSTYPE','--target',str(root)],text=True).strip(),cases=rows,events={k:count(k) for k in ('open','release','backing_register','backing_close','destroy')},limitations=['Three local mapping modes and one forked shared mapping on this ext4/kernel combination only.','No SL7 metadata overrides, sealing, mutation epochs, crash recovery or performance were tested.','Does not establish safety on overlayfs or other backing filesystems.'])
 print(json.dumps(result,indent=2))
