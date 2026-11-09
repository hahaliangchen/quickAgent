"""Snapshot real flow submissions and Codex JS outside the repository; never execute them."""
import argparse,datetime,hashlib,json,pathlib,sqlite3

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-db',type=pathlib.Path,action='append',default=[])
    parser.add_argument('--codex-sessions',type=pathlib.Path,action='append',default=[])
    parser.add_argument('--output',type=pathlib.Path,required=True)
    args=parser.parse_args();rows=[]
    for db in args.app_db:
        with sqlite3.connect(db.resolve().as_uri()+'?mode=ro',uri=True) as conn:
            seen=set()
            for seq,task,model,data in conn.execute("SELECT e.seq,e.task_id,t.model,e.data FROM agent_task_events e JOIN agent_tasks t ON t.id=e.task_id WHERE e.kind='tool/call' AND json_extract(e.data,'$.name')='write_program' ORDER BY e.task_id,e.seq"):
                if model in ['fake','test','mock']:continue
                event=json.loads(data);payload=event.get('arguments',{})
                if isinstance(payload,str):
                    try:payload=json.loads(payload)
                    except ValueError:continue
                source=payload.get('source')
                if isinstance(source,str):
                    rows.append(dict(id=f'app:{db.parent.parent.name}:{task}:{seq}',origin='application',model=model,source=source))
                    seen.add((task,source))
            # Older hosts accepted fenced source without a write_program call.
            for seq,task,model,data in conn.execute("SELECT e.seq,e.task_id,t.model,e.data FROM agent_task_events e JOIN agent_tasks t ON t.id=e.task_id WHERE e.kind='program/start' ORDER BY e.task_id,e.seq"):
                if model in ['fake','test','mock']:continue
                source=json.loads(data).get('source')
                if isinstance(source,str) and (task,source) not in seen:
                    rows.append(dict(id=f'app-start:{db.parent.parent.name}:{task}:{seq}',origin='application',model=model,source=source));seen.add((task,source))
    for directory in args.codex_sessions:
        for path in sorted(directory.rglob('*.jsonl')):
            for line_no,line in enumerate(path.open(encoding='utf-8'),1):
                try:entry=json.loads(line)
                except ValueError:continue
                payload=entry.get('payload') or {}
                if entry.get('type')=='response_item' and payload.get('type')=='custom_tool_call' and payload.get('name')=='exec':
                    source=payload.get('input')
                    if isinstance(source,str):rows.append(dict(id=f'codex:{path.stem}:{line_no}',origin='codex',source=source))
    assert len({r['id'] for r in rows})==len(rows),'duplicate source IDs'
    args.output.parent.mkdir(parents=True,exist_ok=True)
    with args.output.open('w',encoding='utf-8',newline='\n') as output:
        for row in rows:output.write(json.dumps(row,ensure_ascii=False)+'\n')
    counts={origin:sum(r['origin']==origin for r in rows) for origin in sorted({r['origin'] for r in rows})}
    print(json.dumps({'counts':counts,'sha256':hashlib.sha256(args.output.read_bytes()).hexdigest(),'output':str(args.output)},ensure_ascii=False))

if __name__=='__main__':main()
