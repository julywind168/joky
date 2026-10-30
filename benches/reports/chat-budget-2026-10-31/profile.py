import subprocess,time,sys
from pathlib import Path
root=Path(__file__).resolve().parent
version=sys.argv[1]
for room in [1,4]:
 out=root/f'profile-{version}-{room}'
 with (root/f'profile-{version}-{room}.log').open('w') as log:
  p=subprocess.Popen(['python3','benches/chat-workers.py','run','--build',str(root/f'{version}-symbols'),'--output',str(out),'--workers','4','--rooms',str(room),'--clients','16','--messages','500','--warmup','25','--samples','1','--min-seconds','12'],stdout=log,stderr=subprocess.STDOUT)
  sampled=False
  while p.poll() is None:
   lat=out/'latencies.csv'
   if not sampled and lat.exists() and lat.stat().st_size>32768:
    children=subprocess.check_output(['pgrep','-P',str(p.pid)],text=True).split()
    chat=[pid for pid in children if '/app/chat' in subprocess.check_output(['ps','-p',pid,'-o','command='],text=True)]
    if chat:
     subprocess.run(['sample',chat[0],'5','1','-file',str(root/f'profile-{version}-{room}.txt')],check=True,stdout=log,stderr=subprocess.STDOUT)
     sampled=True
   time.sleep(.1)
  assert p.returncode==0 and sampled,(p.returncode,sampled)
 print(version,room,'profile complete',flush=True)
