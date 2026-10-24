import hashlib,json,os,resource,statistics,subprocess,tempfile,time
from pathlib import Path
binary=Path('/sl7/target/release/sl7')
base_binary=Path('/sl7-baseline')
libdir=Path(subprocess.check_output(['rustc','--print','target-libdir'],text=True).strip())
libs=list(libdir.glob('libstd-*.rlib'));assert len(libs)==1
public=libs[0].read_bytes()
with tempfile.TemporaryDirectory(prefix='sl7-pipeline-') as directory:
 root=Path(directory);original=root/'original';edited=root/'edited'
 data=public*16
 original.write_bytes(data)
 changed=bytearray(data);changed[len(changed)//2+123]^=1;edited.write_bytes(changed)
 expected=hashlib.sha256(changed).hexdigest()
 rows=[];reports=[]
 def cmd(store,mode,*args):
  return subprocess.run([str(base_binary if mode==-1 else binary),'--store',str(store),*map(str,args)],env=dict(os.environ,SL7_PIPELINE=str(max(0,mode))),text=True,capture_output=True,check=True)
 for round_id in range(6):
  for mode in ([-1,0,1,2] if round_id%2==0 else [2,1,0,-1]):
   store=root/f'store-{round_id}-{mode}'
   cmd(store,0,'init');seed=json.loads(cmd(store,0,'put','seed','artifact',original).stdout)
   before=resource.getrusage(resource.RUSAGE_CHILDREN);start=time.monotonic()
   run=cmd(store,mode,'put','edit','artifact',edited)
   elapsed=time.monotonic()-start;after=resource.getrusage(resource.RUSAGE_CHILDREN)
   report=json.loads(run.stdout)
   assert ('sl7: chunk pipeline enabled' in run.stderr)==(mode>0)
   assert report['file']['sha256']==expected and report['file']['size']==len(changed)
   assert report['new_chunks'] < seed['new_chunks']
   check=json.loads(cmd(store,0,'check').stdout)
   assert check['files']==2
   packs=sorted(p.name for p in (store/'packs').iterdir())
   sample=dict(round=round_id+1,mode=mode,pipeline=(mode>0),wall_seconds=elapsed,user_seconds=after.ru_utime-before.ru_utime,kernel_seconds=after.ru_stime-before.ru_stime,new_chunks=report['new_chunks'],new_pack_bytes=report['new_pack_bytes'],references=report['references'],pack_ids=packs)
   rows.append(sample);reports.append(report)
   print(json.dumps(sample),flush=True)
 assert all(r==reports[0] for r in reports)
 assert all(r['pack_ids']==rows[0]['pack_ids'] for r in rows)
 summary={str(mode):{key:statistics.median(r[key] for r in rows if r['mode']==mode) for key in ['wall_seconds','user_seconds','kernel_seconds']} for mode in (-1,0,1,2)}
 result=dict(status='passed',kind='Public standard-library artifact repeated sixteen times, with one changed byte; existing chunks preseeded. This is a CLI import microbenchmark, not Cargo timing.',rustc=subprocess.check_output(['rustc','-Vv'],text=True),binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),baseline_binary_sha256=hashlib.sha256(base_binary.read_bytes()).hexdigest(),public_rlib_sha256=hashlib.sha256(public).hexdigest(),original_sha256=hashlib.sha256(data).hexdigest(),edited_sha256=expected,input_bytes=len(data),rows=rows,summary=summary,all_reports_and_packs_identical=True,integrity_checks=len(rows),max_queued_raw_bytes=32*32768)
 print('RESULT '+json.dumps(result),flush=True)
