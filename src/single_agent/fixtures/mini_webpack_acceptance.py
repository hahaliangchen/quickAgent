import json, pathlib, subprocess, sys, tempfile
root=pathlib.Path(sys.argv[1]).resolve(); turn=int(sys.argv[2]); results=[]
cases=[
 (1,'basic',{'entry.js':"import { n } from './value.js'; console.log(n + 1);",'value.js':'export const n = 41;'},'42'),
 (1,'nested',{'entry.js':"import { n } from './lib/a.js'; console.log(n);",'lib/a.js':"import { x } from './b.js'; export const n = x * 2;",'lib/b.js':'export const x = 9;'},'18'),
 (1,'shared',{'entry.js':"import { a } from './a.js'; import { b } from './b.js'; console.log(a + b);",'a.js':"import { n } from './common.js'; export const a=n;",'b.js':"import { n } from './common.js'; export const b=n;",'common.js':"console.log('once'); export const n=2;"},'once\n4'),
 (2,'default_alias',{'entry.js':"import add, { value as v } from './math.js'; console.log(add(v, 3));",'math.js':'export const value=4; export default function add(a,b){return a+b;}'},'7'),
 (2,'reexport',{'entry.js':"import { x } from './barrel.js'; console.log(x);",'barrel.js':"export { x } from './value.js';",'value.js':'export const x=8;'},'8'),
 (2,'json',{'entry.js':"import data from './data.json'; console.log(data.message);",'data.json':'{"message":"hello json"}'},'hello json'),
 (3,'css',{'entry.js':"import './style.css'; console.log('page');",'style.css':'body { color: red; }'},'page'),
 (3,'resolve_extension',{'entry.js':"import { x } from './lib/value'; console.log(x);",'lib/value.js':'export const x=17;'},'17'),
]
for stage,name,files,expected in cases:
 if stage>turn: continue
 with tempfile.TemporaryDirectory(prefix='mini-webpack-case-') as temp:
  p=pathlib.Path(temp)
  for name_,content in files.items():
   f=p/name_; f.parent.mkdir(parents=True,exist_ok=True); f.write_text(content,encoding='utf8')
  out=p/'out'/'bundle.js'
  try:
   build=subprocess.run(['node',str(root/'mini-webpack.js'),str(p/'entry.js'),str(out)],cwd=root,capture_output=True,text=True,encoding='utf-8',errors='replace',timeout=45)
   if build.returncode!=0 or not out.is_file(): raise RuntimeError('build failed: '+(build.stderr or build.stdout)[-1200:])
   run=subprocess.run(['node',str(out)],cwd=p,capture_output=True,text=True,encoding='utf-8',errors='replace',timeout=10)
   if run.returncode!=0: raise RuntimeError('bundle failed: '+run.stderr[-1200:])
   if run.stdout.strip()!=expected: raise RuntimeError('expected '+repr(expected)+' got '+repr(run.stdout.strip()))
   if name=='css' and 'color' not in out.read_text(encoding='utf8') and not any('color' in f.read_text(encoding='utf8') for f in (p/'out').glob('*.css')): raise RuntimeError('CSS absent from output')
   results.append({'case':name,'stage':stage,'passed':True})
  except Exception as e: results.append({'case':name,'stage':stage,'passed':False,'error':str(e)})
if turn>=4:
 with tempfile.TemporaryDirectory(prefix='mini-webpack-error-') as temp:
  p=pathlib.Path(temp); (p/'entry.js').write_text("import { missing } from './missing.js'; console.log(missing);",encoding='utf8')
  try:
   r=subprocess.run(['node',str(root/'mini-webpack.js'),str(p/'entry.js'),str(p/'bundle.js')],cwd=root,capture_output=True,text=True,encoding='utf-8',errors='replace',timeout=45)
   ok=r.returncode!=0 and 'missing' in (r.stdout+r.stderr).lower()
   results.append({'case':'missing_module_error','stage':4,'passed':ok,'error':'' if ok else 'expected nonzero exit and missing path'})
  except Exception as e: results.append({'case':'missing_module_error','stage':4,'passed':False,'error':str(e)})
print(json.dumps({'passed':sum(x['passed'] for x in results),'total':len(results),'cases':results},ensure_ascii=False))
