import subprocess
from pathlib import Path
root=Path(__file__).resolve().parent
repo=root.parents[2]
for repeat,(workers,rooms,versions) in enumerate([
 ('1,4','1,4',['before','after']),
 ('4,1','4,1',['after','before']),
 ('1,4','1,4',['before','after']),
],1):
 for scene in ['matrix','single']:
  for version in versions:
   build=root/'after' if version=='after' else root.parent/'chat-byte-transfer/moved'
   output=root/f'{scene}-{repeat}-{version}'
   args=['python3',str(repo/'benches/chat-workers.py'),'run','--build',str(build),'--output',str(output),
    '--workers',workers if scene=='matrix' else '1','--rooms',rooms if scene=='matrix' else '1',
    '--clients','16' if scene=='matrix' else '1','--messages','250' if scene=='matrix' else '5000',
    '--warmup','25','--samples','1','--min-seconds','3']
   print(output.name,flush=True)
   subprocess.run(args,check=True,cwd=repo)
