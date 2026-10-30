import re,csv,sys
from collections import Counter
from pathlib import Path
root=Path(__file__).resolve().parent
rows=[]
for path in sorted(root.glob('profile-*.txt')):
 stack=[]; groups=Counter(); active=False; thread=''
 for line in path.read_text().splitlines():
  if line=='Call graph:': active=True; continue
  if line.startswith('Total number in stack'): break
  if not active: continue
  m=re.match(r'([ +!:|]*)(\d+) (.*)',line)
  if not m: continue
  depth=len(m[1]); count=int(m[2]); name=m[3]
  while stack and stack[-1][0]>=depth: stack.pop()
  if name.startswith('Thread_'): thread=name
  if name.startswith('__psynch_mutexwait '):
   callers=[s for _,s in stack if '4joky' in s]
   symbol=callers[-1].split('  (in ')[0] if callers else 'non-Joky'
   groups[(thread,symbol)]+=count
  stack.append((depth,name))
 sums=Counter()
 for (thread,symbol),count in groups.items():
  group='Lease::resize' if '7storage' in symbol and '5Lease6resize' in symbol else 'Lease::drop' if '7storage' in symbol and '5Lease' in symbol and '4drop' in symbol else 'other'
  sums[group]+=count
  rows.append(dict(profile=path.stem,thread=thread,symbol=symbol,group=group,mutex_wait_samples=count))
 print(path.stem,dict(sums),flush=True)
if rows:
 with (root/'profile-mutex.csv').open('w') as f:
  w=csv.DictWriter(f,fieldnames=rows[0],lineterminator='\n'); w.writeheader(); w.writerows(rows)
