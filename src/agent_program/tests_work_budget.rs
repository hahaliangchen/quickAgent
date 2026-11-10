use super::*;

#[test]
fn work_budget_builtin_cost_is_not_its_interpreter_step_count() {
    let mut run = budgeted_runtime("function main(){return Array(200).fill(3).map(n=>n+1).reduce((a,b)=>a+b,0);}", 8000);
    assert_eq!(returned(run.clone()), json!(800));
    run.next();
    let snapshot = serde_json::to_value(run).unwrap();
    assert!(snapshot["total_vm_steps"].as_u64().unwrap() > snapshot["total_instructions"].as_u64().unwrap() * 3);
}

#[test]
fn work_budget_builtin_exhaustion_reports_user_source() {
    let mut run = budgeted_runtime("function main(){\nreturn Array(10000).fill(1);\n}", 64);
    let State::Failed(error) = run.next() else {panic!("builtin work must remain bounded")};
    assert_eq!(error["code"], "INSTRUCTION_BUDGET_EXHAUSTED");
    assert_eq!(error["location"]["line"], 2);
    assert_eq!(error["hot_lines"][0]["function"], "main");
    assert_eq!(error["hot_lines"][0]["line"], 2);
    assert!(!error["hot_lines"].to_string().contains("$$"));
}

#[test]
fn work_budget_user_callback_loop_cannot_hide_inside_builtin() {
    let mut run = budgeted_runtime("function main(){\nreturn [2,1].sort(function compare(){\nwhile(true){}\n});\n}", 100);
    let State::Failed(error) = run.next() else {panic!("user comparator must be metered")};
    assert_eq!(error["code"], "INSTRUCTION_BUDGET_EXHAUSTED");
    assert_eq!(error["hot_lines"][0]["line"], 3);
    assert!(!error["hot_lines"].to_string().contains("$$"));
}

#[test]
fn work_budget_wall_heartbeat_uses_physical_steps_not_billing() {
    let mut run = budgeted_runtime("function main(){return Array(10000).fill(1);}", 1000);
    run.set_test_deadline_after_steps(128);
    let State::Failed(error) = run.next() else {panic!("wall time must be checked inside builtins")};
    assert_eq!(error["code"], "PROGRAM_TIME_BUDGET_EXHAUSTED");
    assert!(error["total_vm_steps"].as_u64().unwrap() >= 256);
    assert!(error["used"].as_u64().unwrap() < 128);
}

#[test]
fn work_budget_call_site_and_counters_survive_suspended_builtin() {
    let mut run = budgeted_runtime("function main(){\nconst a=Array(200);Object.defineProperty(a,'0',{get(){io.read({});return 1;},configurable:true});\nreturn a.map(x=>x);\n}", 5000);
    assert!(matches!(run.next(), State::Action(_)));
    let mut state = serde_json::to_value(&run).unwrap();
    let steps = state["total_vm_steps"].as_u64().unwrap();
    state["fuel"] = json!(64); state["segment_limit"] = json!(64);
    let mut restored: Runtime = serde_json::from_value(state).unwrap();
    assert!(matches!(restored.next(), State::Action(_)));
    assert_eq!(serde_json::to_value(&restored).unwrap()["total_vm_steps"], steps);
    restored.resume(json!({}), false).unwrap();
    let State::Failed(error) = restored.next() else {panic!("restored builtin work must be metered")};
    assert_eq!(error["code"], "INSTRUCTION_BUDGET_EXHAUSTED");
    assert_eq!(error["location"]["line"], 3);
    assert!(!error["hot_lines"].to_string().contains("$$"));
    assert!(error["total_vm_steps"].as_u64().unwrap() > steps);
}

#[test]
fn work_budget_native_mutations_preserve_property_semantics() {
    assert_eq!(returned(runtime(r#"function main(){
      const a=[],o={};a.push(1,2);a[4]=5;o.x=3;delete o.x;o.x=4;
      let setter=0;Object.defineProperty(Array.prototype,'5',{set(v){setter=v},configurable:true});
      const length=a.push(7);delete Array.prototype[5];
      const sealed=Object.seal([1]);sealed[0]=9;let blocked=0;
      for(const f of [()=>Object.freeze({}).x=1,()=>Object.freeze([]).push(1),()=>Object.defineProperty([],'length',{writable:false}).push()]){
        try{f()}catch(e){if(e instanceof TypeError)blocked++}
      }
      const proto={set x(v){this.y=v+1}},child=Object.create(proto);child.x=6;
      return [a,length,setter,o.x,sealed,blocked,child.y];
    }"#)), json!([[1,2,null,null,5,null],6,7,4,[9],3,7]));
}

#[test]
fn work_budget_collection_follows_heap_growth_not_every_heartbeat() {
    let mut run = budgeted_runtime("function main(){let k=0;for(let i=0;i<4000;i++){k+=[i].length;}return k;}", 2_000_000);
    assert!(matches!(run.next(), State::Returned(_)));
    let steps = serde_json::to_value(&run).unwrap()["total_vm_steps"].as_u64().unwrap() as usize;
    assert!(steps / 256 > 40, "the program must span many heartbeats, took {steps}");
    if super::super::heap::gc_stress() { return; }
    assert!(run.gc_collections() * 4 < steps / 256, "{} collections in {steps} steps", run.gc_collections());
}

#[test]
fn work_budget_snapshot_before_host_effect_holds_no_loop_garbage() {
    let nodes = |source: &str| {
        let mut run = budgeted_runtime(source, 2_000_000);
        assert!(matches!(run.next(), State::Action(_)));
        serde_json::to_value(&run).unwrap()["heap"]["nodes"].as_object().unwrap().len()
    };
    let idle = nodes("function main(){let k=0;return io.read({path:'a'});}");
    let churned = nodes("function main(){let k=0;for(let i=0;i<3000;i++){k+=[{i}].length;}return io.read({path:'a'});}");
    assert!(churned <= idle + 16, "idle {idle}, churned {churned}");
}

#[test]
fn work_budget_snapshot_before_required_library_call_holds_no_loop_garbage() {
    let nodes = |source: &str| {
        let mut run = budgeted_runtime(source, 2_000_000);
        assert!(matches!(run.next(), State::Action(_)));
        run.resume(json!({"library":"ppt","methods":{"get_design":"ppt_master__get_design"}}), false).unwrap();
        let State::Action(action) = run.next() else {panic!("library call must reach the host")};
        assert!(matches!(action.kind, ActionKind::Tool { .. }));
        serde_json::to_value(&run).unwrap()["heap"]["nodes"].as_object().unwrap().len()
    };
    let idle = nodes("function main(){const ppt=require('ppt');let k=0;return ppt.get_design({});}");
    let churned = nodes("function main(){const ppt=require('ppt');let k=0;for(let i=0;i<3000;i++){k+=[{i}].length;}return ppt.get_design({});}");
    assert!(churned <= idle + 16, "idle {idle}, churned {churned}");
}

#[test]
fn work_budget_snapshot_before_async_tool_holds_garbage_made_after_enqueue() {
    let nodes = |source: &str| {
        let mut run = budgeted_runtime(source, 2_000_000);
        assert!(matches!(run.next(), State::Action(_)));
        serde_json::to_value(&run).unwrap()["heap"]["nodes"].as_object().unwrap().len()
    };
    let head = "import * as io from 'host:io/promises'; async function main(){const p=io.read({path:'a'});let k=0;";
    let idle = nodes(&format!("{head}return await p;}}"));
    let churned = nodes(&format!("{head}for(let i=0;i<3000;i++){{k+=[{{i}}].length;}}return await p;}}"));
    assert!(churned <= idle + 16, "idle {idle}, churned {churned}");
}

#[test]
fn work_budget_enqueuing_async_tools_does_not_collect_per_call() {
    let mut run = budgeted_runtime("import * as io from 'host:io/promises'; async function main(){const all=[];for(let i=0;i<40;i++)all.push(io.read({path:'a'+i}));return await Promise.all(all);}", 2_000_000);
    assert!(matches!(run.next(), State::Action(_)));
    if super::super::heap::gc_stress() { return; }
    assert!(run.gc_collections() <= 2, "{} collections", run.gc_collections());
}

#[test]
fn work_budget_large_payload_garbage_triggers_collection_with_few_nodes() {
    let mut run = budgeted_runtime("function main(){let n=0;for(let i=0;i<200;i++){const s='x'.repeat(1048576);n+=s.length;}return n;}", 2_000_000);
    assert_eq!(returned(run.clone()), json!(209715200));
    assert!(matches!(run.next(), State::Returned(_)));
    assert!(run.gc_collections() >= 4, "{} collections", run.gc_collections());
}

#[test]
fn work_budget_values_survive_scheduled_collections_and_resume() {
    let mut run = budgeted_runtime(r#"function main(){
      const keep=[];const seen=new Map();
      for(let i=0;i<9000;i++){const o={i};if(i%3000===0){keep.push(o);seen.set(o,()=>o.i);}}
      const before=io.read({path:'a'});
      for(let i=0;i<9000;i++){[i];}
      return [keep.map(o=>seen.get(o)()),before.n];
    }"#, 2_000_000);
    assert!(matches!(run.next(), State::Action(_)));
    let mut restored: Runtime = serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    restored.resume(json!({"n":7}), false).unwrap();
    assert_eq!(returned(restored), json!([[0,3000,6000],7]));
}

/// Replays exact failed segments without model calls, shell or source writes.
#[test]
#[ignore = "recorded local self-review workloads; WORK_BUDGET_REPLAY_DIR required"]
fn work_budget_replay_recorded_failures() {
    use std::collections::VecDeque;
    let root = std::path::PathBuf::from(std::env::var("WORK_BUDGET_REPLAY_DIR").unwrap());
    let conn = rusqlite::Connection::open_with_flags(root.join("workspace/.codex-workspace-mcp/codex_state.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let events: Vec<(i64,String,Value)> = conn.prepare("SELECT seq,kind,data FROM agent_task_events ORDER BY seq").unwrap()
        .query_map([], |r| Ok((r.get(0)?,r.get(1)?,serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap())))
        .unwrap().map(Result::unwrap).collect();
    let mut reports=Vec::new();
    for (start, end, name) in [(1033,1040,"surveyArchitecture"),(2123,2244,"auditErrorHandling")] {
        let source_event = events.iter().find(|e|e.0==start).unwrap();
        let source = source_event.2["source"].as_str().unwrap();
        let descriptor = events.iter().rev().find_map(|(seq,kind,data)| {
            if *seq>=start || kind!="tool/call" || data["name"]!="create_task" {return None}
            let args: Value=serde_json::from_str(data["arguments"].as_str().unwrap()).unwrap();
            (args["program_task"]["name"]==name).then(||args["program_task"].clone())
        }).unwrap();
        let node = source_event.2["task_node_id"].clone();
        let mut replies=VecDeque::new();
        for (seq,kind,data) in &events {
            if *seq<=start || *seq>=end || kind!="tool/call" || data["task_node_id"]!=node || data["program_location"].is_null() {continue}
            let result=events.iter().find(|(_,kind,result)|kind=="tool/result" && result["message"]["toolCallId"]==data["callId"]).unwrap();
            let args: Value=serde_json::from_str(data["arguments"].as_str().unwrap()).unwrap();
            replies.push_back((data["name"].as_str().unwrap().to_owned(),args,result.2["meta"]["result"].clone()));
        }
        let program=compile_body(source,name).unwrap();
        let mut run=Runtime::task_program(program,name,descriptor["arguments"].as_array().unwrap().clone()).unwrap();
        let started=std::time::Instant::now();let mut consumed=0;
        let outcome=loop {
            match run.next() {
                State::Failed(error)=>break json!({"failed":error}),
                State::Returned(value)=>break json!({"returned":value}),
                State::Waiting=>panic!("unexpected timer/async wait"),
                State::Action(action)=>{
                    if let ActionKind::Require{library}=&action.kind {
                        let base=library.trim_end_matches("/promises");
                        let methods=libraries::builtin()[base].clone();
                        run.resume(json!({"library":library,"methods":methods}),false).unwrap();continue;
                    }
                    let Some((name,args,value))=replies.pop_front() else {break json!({"passed_failed_segment":true,"next_action":format!("{:?}",action.kind)})};
                    match &action.kind {
                        ActionKind::Tool{name:actual,args:actual_args}=>{assert_eq!(actual,&name);assert_eq!(actual_args,&args);},
                        ActionKind::ParallelReads{..}=>assert_eq!(name,"parallel_read_batch"),
                        kind=>panic!("unexpected replay action: {kind:?}"),
                    }
                    consumed+=1;run.resume(value,false).unwrap();
                }
            }
        };
        let state=serde_json::to_value(run).unwrap();
        let mut hot_lines=Vec::new();
        for (function, lines) in state["line_instructions"].as_object().unwrap() {
            assert!(!function.starts_with("$$"), "builtin charges must belong to user code");
            for (line, count) in lines.as_object().unwrap() {
                hot_lines.push(json!({"function":function,"line":line.parse::<usize>().unwrap(),"instructions":count}));
            }
        }
        hot_lines.sort_by_key(|line|std::cmp::Reverse(line["instructions"].as_u64().unwrap()));
        hot_lines.truncate(8);
        reports.push(json!({"name":name,"seconds":started.elapsed().as_secs_f64(),"replies":consumed,
            "remaining_replies":replies.len(),"used":state["segment_limit"].as_u64().unwrap()-state["fuel"].as_u64().unwrap(),
            "limit":state["segment_limit"],"total_work":state["total_instructions"],"total_vm_steps":state["total_vm_steps"],
            "hot_lines":hot_lines,"outcome":outcome}));
    }
    let report=json!(reports);eprintln!("WORK_BUDGET_REPLAY {report}");
    if let Ok(path)=std::env::var("WORK_BUDGET_REPLAY_REPORT") {std::fs::write(path,serde_json::to_vec_pretty(&report).unwrap()).unwrap();}
    for case in report.as_array().unwrap() {
        assert_eq!(case["outcome"]["passed_failed_segment"], true, "{case}");
        assert_eq!(case["remaining_replies"], 0);
    }
}
