#[test]
fn block_scopes_survive_suspension_and_restore() {
    let source = r#"
function main(){
  const rows = [];
  for (let i = 0; i < 2; i++) {
    const label = 'item' + i;
    { const doc = io.read({path: label}); rows.push(label + ':' + doc.content); }
  }
  return rows;
}"#;
    let mut run = runtime(source);
    for content in ["A", "B"] {
        assert!(matches!(run.next(), State::Action(_)));
        assert!(run.named_bindings().contains_key("label"));
        run = serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
        run.resume(json!({"content": content}), false).unwrap();
    }
    assert_eq!(returned(run), json!(["item0:A", "item1:B"]));
}

#[test]
fn closures_keep_their_own_state_and_share_updates() {
    let source = r#"
function makeCounter(){ let count = 0; return () => { count++; return count; }; }
function main(){
  const a = makeCounter();
  const b = makeCounter();
  a(); a(); b();
  let shared = 1;
  const read = () => shared;
  shared = 2;
  return [a(), b(), read(), typeof a, a.name, makeCounter.name];
}"#;
    assert_eq!(returned(runtime(source)), json!([3, 2, 2, "function", "", "makeCounter"]));
}

#[test]
fn loop_closures_capture_each_let_iteration_and_one_var() {
    let source = r#"
function main(){
  const fns = [];
  for (let i = 0; i < 3; i++) { fns.push(() => i); }
  const vars = [];
  for (var j = 0; j < 3; j++) { vars.push(() => j); }
  const items = [];
  for (const item of ['a', 'b']) { items.push(() => item); }
  return [fns[0](), fns[1](), fns[2](), vars[0](), vars[2](), items[0](), items[1]()];
}"#;
    assert_eq!(returned(runtime(source)), json!([0, 1, 2, 3, 3, "a", "b"]));
}

#[test]
fn functions_are_values_in_arrays_objects_and_expressions() {
    let source = r#"
function main(){
  const ops = { double(x) { return x * 2; }, inc: x => x + 1 };
  const list = [ops.double, ops.inc, helper];
  const fact = function f(n) { return n <= 1 ? 1 : n * f(n - 1); };
  { function local(){ return 'block'; } var fromBlock = local(); }
  const toText = String;
  return [ops.double(4), ops.inc(4), list[0](5), list[2](1), (x => x * 10)(3), fact(5),
          fromBlock, hoisted(), toText(12), [[1, 2], [3]].map(row => row.map(x => x * 10)),
          [1, 2].map(x => () => x).map(f => f())];
  function hoisted(){ return 'hoisted'; }
}
function helper(x){ return x + 100; }
"#;
    assert_eq!(
        returned(runtime(source)),
        json!([8, 5, 10, 101, 30, 120, "block", "hoisted", "12", [[10, 20], [30]], [1, 2]])
    );
    // Functions are not data: they leave objects and become null in arrays.
    assert_eq!(
        returned(runtime("function main(){return {f: () => 1, x: 1, list: [() => 2, 3]};}")),
        json!({"x": 1, "list": [null, 3]})
    );
    // Calling something that is not a function is a TypeError the flow can catch.
    assert_eq!(
        returned(runtime("function main(){const x = {}; try { x.missing(); } catch (error) { return error.name; }}")),
        json!("TypeError")
    );
}

#[test]
fn ordinary_functions_take_javascript_arity_and_ai_functions_stay_exact() {
    assert_eq!(
        returned(runtime("function pair(a, b){ return [a, b]; } function main(){ return [pair(1), pair(1, 2, 3)]; }")),
        json!([[1, null], [1, 2]])
    );
    let error = compile("function main(){ return review(1, 2); } /** @virtual Review one item; return {passed}. */ function review(item){}")
        .unwrap_err()
        .to_string();
    assert!(error.contains("expects 1 arguments, got 2"), "{error}");
}

#[test]
fn closures_and_tool_values_survive_suspension() {
    let source = r#"
function main(){
  let total = 0;
  const add = n => { total += n; return total; };
  const load = path => io.read({path});
  const read = io.read;
  add(1);
  const first = load('a');
  add(first.content.length);
  const second = read({path: 'b'});
  return [total, add(second.content.length)];
}"#;
    let mut run = runtime(source);
    for (path, content) in [("a", "abc"), ("b", "xy")] {
        assert!(matches!(
            run.next(),
            State::Action(Action { kind: ActionKind::Tool { ref name, ref args }, .. })
                if name == "read_file" && args["path"] == path
        ));
        run = serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
        run.resume(json!({"content": content}), false).unwrap();
    }
    assert_eq!(returned(run), json!([4, 6]));
}

#[test]
fn model_written_five_page_acceptance_flow_compiles() {
    // Written by a model without guidance on the subset (2026-10-28). It
    // repeats `let i` and `catch (error)` within functions.
    let source = include_str!("../fixtures/model-five-page-acceptance.js");
    let program = compile(source).unwrap();
    assert!(program.functions["proposeMinimalEdits"].virtual_task);
}

#[test]
fn model_written_five_page_acceptance_flow_runs_with_recorded_receipts() {
    let source = include_str!("../fixtures/model-five-page-acceptance.js");
    let mut run = top_level_task(compile(source).unwrap());
    let pages = ["01-overview", "02-program-flow", "03-task-context", "04-parallel-agents", "05-verification"];
    let requirements: Vec<_> = pages.iter().enumerate().map(|(i, page)| json!({
        "file": format!("agent-design-html/{page}.html"), "purpose": "Document the design",
        "requirements": [{"id": format!("req-{i}"), "criterion": "Has content", "evidence": [{"file": "docs/README.md", "quote": "reference"}]}]
    })).collect();
    let mut reads = 0;
    let mut reviews = 0;
    let mut models = 0;
    for _ in 0..40 {
        let action = match run.next() {
            State::Action(action) => action,
            State::Returned(_) => break,
            other => panic!("fixture failed: {other:?}"),
        };
        let receipt = match action.kind {
            ActionKind::Tool { name, args } if name == "read_file" => {
                reads += 1;
                if args["path"].as_str().unwrap().ends_with(".html") {
                    json!({"content":"<!doctype html><html><head><title>Page</title></head><body><h1>Page</h1></body></html>"})
                } else { json!({"content":"reference"}) }
            }
            ActionKind::Call { name, .. } if name == "deriveAcceptanceModel" => {
                models += 1;
                json!({"pageRequirements":requirements,"crossPageRules":[],"terminology":[]})
            }
            ActionKind::Call { name, .. } if name == "reviewPageContent" || name == "reviewCrossPageConsistency" => {
                reviews += 1;
                json!([])
            }
            other => panic!("unexpected fixture action: {other:?}"),
        };
        run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(receipt, false).unwrap();
    }
    assert_eq!((reads, models, reviews), (8, 1, 12));
    assert!(matches!(run.next(), State::Returned(_)));
}

#[test]
fn callback_values_and_multistatement_callbacks_follow_js_semantics() {
    let source = r#"function main() {
      const values = [0, 1, 2, 3];
      const selected = values.filter(Boolean);
      return selected.map(value => {
        const doubled = value * 2;
        return [doubled].map(x => x + 1)[0];
      });
    }"#;
    let run = runtime(source);
    let run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!([3, 5, 7]));
}

#[test]
fn callback_tool_calls_resume_after_serialization() {
    let source = r#"function main() {
      return ['a', 'b'].map(path => {
        const file = io.read({path});
        return file.content;
      });
    }"#;
    let mut run = runtime(source);
    for (path, content) in [("a", "first"), ("b", "second")] {
        assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref name, ref args }, .. }) if name == "read_file" && args["path"] == path));
        run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(json!({"content": content}), false).unwrap();
    }
    assert_eq!(returned(run), json!(["first", "second"]));
}

#[test]
fn prelude_sort_and_array_from_use_function_values() {
    let source = "function main(){ const order=(a,b)=>a.key-b.key; const rows=[{key:2,id:'a'},{key:1,id:'b'},{key:2,id:'c'}]; return [rows.toSorted(order).map(x=>x.id), Array.from([0,1,2], Boolean)]; }";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!([["b", "a", "c"], [false, true, true]]));
}

#[test]
fn sort_comparator_can_suspend_and_preserves_stable_order() {
    let source = "function main(){return [{v:2,id:'a'},{v:1,id:'b'},{v:2,id:'c'}].toSorted((a,b)=>{io.read({path:'compare'});return a.v-b.v;}).map(x=>x.id);}";
    let mut run = runtime(source);
    let mut calls = 0;
    loop {
        match run.next() {
            State::Action(Action { kind: ActionKind::Tool { name, args }, .. }) => {
                assert_eq!(name, "read_file");
                assert_eq!(args["path"], "compare");
                calls += 1;
                run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
                run.resume(json!({"content":"ok"}), false).unwrap();
            }
            State::Returned(result) => { assert_eq!(result, json!(["b", "a", "c"])); break; }
            other => panic!("unexpected state: {other:?}"),
        }
    }
    assert!(calls > 0);
}

#[test]
fn prelude_hash_and_internal_names_are_enforced() {
    assert!(compile("function main(){ return $$has([], 0); }").is_err());
    let mut run = runtime("function main(){ return [1].map(x=>x); }");
    run.program.prelude_hash = "old".into();
    assert!(matches!(run.next(), State::Failed(_)));
}

#[test]
fn replacement_callbacks_receive_captures_and_survive_tool_suspension() {
    let source = r#"function main() {
      return 'a1 b2'.replace(/([a-z])(\d)/g, (whole, letter, digit, offset, text) => {
        const file = io.read({path: letter});
        return file.content + digit + offset + text.length;
      });
    }"#;
    let mut run = runtime(source);
    for (path, content) in [("a", "A"), ("b", "B")] {
        assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["path"] == path));
        run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(json!({"content": content}), false).unwrap();
    }
    assert_eq!(returned(run), json!("A105 B235"));
    assert_eq!(returned(runtime("function main(){return 'ab'.replaceAll('', (_, i)=>String(i));}")), json!("0a1b2"));
    assert_eq!(returned(runtime("function main(){try{return 'aa'.replaceAll(/a/, x=>x);}catch(e){return e.name;}}")), json!("TypeError"));
}

#[test]
fn internal_prelude_frames_do_not_hide_or_expose_ai_scope() {
    let source = "function main(){const userValue='visible';return [1].map(x=>inspect(x));} /** @virtual Inspect input and return a result. */ function inspect(x){}";
    let mut run = start_task(compile(source).unwrap(), vec![]);
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Call { ref name, .. }, .. }) if name == "inspect"));
    let bindings = run.named_bindings();
    assert_eq!(bindings["userValue"], "visible");
    assert_eq!(bindings["x"], 1);
    assert!(!bindings.contains_key("a") && !bindings.contains_key("f"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!("ok"), false).unwrap();
    assert_eq!(returned(run), json!(["ok"]));
}

#[test]
fn quickjs_closure_fixture_works_through_array_callback_after_restore() {
    let run = runtime(include_str!("../fixtures/quickjs-closure-callback.js"));
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!(10));
}

#[test]
fn m4_destructuring_callback_and_defaults_survive_suspension() {
    let source = r#"function main() {
      const input = {a: 2, b: 3};
      const result = Object.entries(input).map(([key, value]) => {
        const {content: extra = 'missing'} = io.read({path:key});
        return [key, value + extra.length];
      });
      return result;
    }"#;
    let mut run = runtime(source);
    for path in ["a", "b"] {
        assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["path"] == path));
        run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(json!({"content":"x"}), false).unwrap();
    }
    assert_eq!(returned(run), json!([["a",3],["b",4]]));
}

#[test]
fn m4_function_and_catch_patterns_use_defaults() {
    let source = r#"function read({name, count = 2}) { return name + count; }
function main(){ try { throw {message:'bad'}; } catch ({message, code = 4}) { return [read({name:'n'}), message, code]; } }"#;
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!(["n2", "bad", 4]));
    assert_eq!(returned(runtime("function f(a=b,b){return a;}function main(){try{f(undefined,2);}catch(e){return e.name;}}")), json!("ReferenceError"));
    assert_eq!(returned(runtime("function main(){try{const {}=null;}catch(e){return e.name;}}")), json!("TypeError"));
    assert_eq!(returned(runtime("function main(){try{const []=4;}catch(e){return e.name;}}")), json!("TypeError"));
}

#[test]
fn m4_defaults_do_not_see_body_var_bindings() {
    let source = "const x=9;function read(a=x){var x=1;return a;}io.write({path:'out',content:String(read())});";
    let mut run = top_level_with(source, vec![json!({})]);
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["content"] == "9"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert!(matches!(run.next(), State::Returned(_)));
}

#[test]
fn m4_parameter_closures_keep_parameter_environment() {
    let source = "function f(a=1,get=()=>a){var a=2;return [a,get()];}function main(){return f();}";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!([2,1]));
    assert_eq!(returned(runtime("function f(a=1){function a(){return 3;}return a();}function main(){return f();}")), json!(3));
    let source = "const x=9;const f=function self(get=()=>self){var self=1;return get()===f;};io.write({path:'result',content:String(f())});";
    let mut run = top_level_with(source, vec![json!({})]);
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["content"] == "true"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert!(matches!(run.next(), State::Returned(_)));
}

#[test]
fn m4_parameter_initializer_can_suspend_before_body_scope() {
    let source = "const path='outer';function read(a=io.read({path})){let path='inner';return [a.content,path];}io.write({path:'result',content:JSON.stringify(read())});";
    let mut run = top_level_with(source, vec![json!({})]);
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["path"] == "outer"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"content":"ok"}), false).unwrap();
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["content"] == "[\"ok\",\"inner\"]"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert!(matches!(run.next(), State::Returned(_)));
}

#[test]
fn m4_older_snapshot_without_parameter_scope_metadata_resumes() {
    let mut run = runtime("function main(){const result=io.read({path:'legacy'});return result.content;}");
    assert!(matches!(run.next(), State::Action(_)));
    let mut saved = serde_json::to_value(run).unwrap();
    for function in saved["program"]["functions"].as_object_mut().unwrap().values_mut() {
        let fields = function.as_object_mut().unwrap();
        fields.remove("parameter_scope");
        fields.remove("parameter_lexical");
    }
    let mut restored: Runtime = serde_json::from_value(saved).unwrap();
    restored.resume(json!({"content":"ok"}), false).unwrap();
    assert_eq!(returned(restored), json!("ok"));
}

#[test]
fn m5_async_functions_resume_after_caller_continues() {
    let source = "async function main(){const order=[];async function work(){order.push('before');await 0;order.push('after');}const pending=work();order.push('caller');await pending;return order;}";
    let run = runtime(source);
    let run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!(["before","caller","after"]));
}

#[test]
fn m4_optional_function_call_skips_arguments_and_survives_snapshot() {
    let source = "function main(){ let f=null; let n=0; const missing=f?.(n++); f=x=>x+1; return [missing, n, f?.(2)]; }";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!([null, 0, 3]));
}

#[test]
fn m4_for_in_enumerates_keys_across_suspensions() {
    let source = "function main(){let result=[];for(const key in {b:2,a:1}){io.read({path:key});result.push(key);}return result;}";
    let mut run = runtime(source);
    for key in ["b", "a"] {
        assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["path"] == key));
        run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(json!({}), false).unwrap();
    }
    assert_eq!(returned(run), json!(["b", "a"]));
    assert_eq!(returned(runtime("function main(){const obj={a:1,b:2};const keys=[];for(const key in obj){keys.push(key);delete obj.b;}return keys;}")), json!(["a"]));
    assert_eq!(returned(runtime("function main(){let key;let output='';for(key in {x:1,y:2})output+=key;return [output,key];}")), json!(["xy","y"]));
}

#[test]
fn m4_switch_falls_through_and_continue_targets_enclosing_loop() {
    let source = "function main(){let out='';for(let i=0;i<4;i++){switch(i){case 0:out+='a';break;case 1:continue;case 2:out+='b';default:out+='c';}out+='x';}return out;}";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!("axbcxcx"));
    let run = runtime(include_str!("../fixtures/quickjs-switch-loop.js"));
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!(true));
}

#[test]
fn m4_labeled_break_and_continue_leave_nested_scopes() {
    let source = "function main(){let n=0;outer:for(let i=0;i<3;i++){for(let j=0;j<3;j++){if(j===1)continue outer;n++;}}block:{n++;break block;n+=100;}return n;}";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!(4));
}

#[test]
fn m4_destructuring_rest_preserves_values() {
    let source = "function main(){const [first,...tail]=[1,2,3];const key='a';const {[key]:picked,...rest}={a:4,b:5};return [first,tail,picked,rest];}";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!([1,[2,3],4,{"b":5}]));
}

#[test]
fn m4_literal_spread_preserves_evaluation_order() {
    let source = "function main(){const data={a:1};const values=[0,...[1,2],3];const obj={...data,b:2};data.a=9;return [values,obj];}";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!([[0,1,2,3],{"a":1,"b":2}]));
}

#[test]
fn m4_spread_call_preserves_argument_evaluation_order() {
    let source = "function pack(a,b,c){return [a,b,c];}function main(){let n=0;return pack(++n,...[n+1,++n]);}";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!([1,2,2]));
}

#[test]
fn m4_rest_parameters_keep_only_remaining_arguments_across_resume() {
    let source = "function collect(first,...rest){io.read({path:'wait'});return [first,rest];}function main(){return [collect(1,2,3),collect(4)];}";
    let mut run = runtime(source);
    for _ in 0..2 {
        assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { .. }, .. })));
        run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(json!({}), false).unwrap();
    }
    assert_eq!(returned(run), json!([[1,[2,3]],[4,[]]]));
}

#[test]
fn m4_spread_method_and_async_calls_preserve_values() {
    let source = "async function add(a,b){return a+b;} async function main(){const rows=[1];rows.push(...[2,3]);return [rows,await add(...[4,5])];}";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!([[1,2,3],9]));
    assert_eq!(returned(runtime("function main(){let f=null;let n=0;const a=f?.(...[++n]);f=(x)=>x;return [a,n,f?.(...[2])];}")), json!([null,0,2]));
    assert_eq!(returned(runtime("function main(){const o={f(x){return 'old';}};function change(){o.f=(x)=>'new';return [1];}return o.f(...change());}")), json!("old"));
    assert_eq!(returned(runtime("function main(){const items=[1];const key='push';items[key](...[2,3]);return items;}")), json!([1,2,3]));
}

#[test]
fn m4_finally_runs_for_return_throw_and_loop_jumps() {
    let source = r#"function result(){try{return 2;}finally{io.read({path:'return'});}}
function main(){let log='';for(let i=0;i<2;i++){try{if(i===0)continue;break;}finally{log+='f';}}
try{throw 'bad';}catch(e){log+=e;}finally{log+='z';}
return [result(),log];}"#;
    let mut run = runtime(source);
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["path"] == "return"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert_eq!(returned(run), json!([2,"ffbadz"]));
}

#[test]
fn m4_finally_can_override_return_and_runs_on_uncaught_throw() {
    let source = "function f(){try{return 1;}finally{return 2;}}function main(){try{try{throw 'x';}finally{io.read({path:'cleanup'});}}catch(e){return [f(),e];}}";
    let mut run = runtime(source);
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["path"] == "cleanup"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert_eq!(returned(run), json!([2,"x"]));
}

#[test]
fn m4_finally_preserves_outer_handlers_and_nested_return_order() {
    let source = r#"function main(){let log='';try{for(let i=0;i<2;i++){try{break;}finally{log+='a';}}throw 'caught';}catch(e){log+=e;}
function nested(){try{try{return 7;}finally{log+='b';}}finally{log+='c';}}return [nested(),log];}"#;
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!([7,"acaughtbc"]));
    assert_eq!(returned(runtime("function f(){try{return 1;}finally{throw 'cleanup';}}function main(){try{return f();}catch(e){return e;}}")), json!("cleanup"));
    assert_eq!(returned(runtime("function main(){for(let i=0;i<1;i++){try{return 1;}finally{break;}}return 2;}")), json!(2));
}

#[test]
fn m4_finally_runs_after_failed_tool_and_preserves_error_on_restore() {
    let source = "function main(){try{try{io.read({path:'bad'});}finally{io.write({path:'cleanup',content:'done'});}}catch(e){return e.message;}}";
    let mut run = runtime(source);
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref name, .. }, .. }) if name == "read_file"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"message":"failed"}), true).unwrap();
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref name, .. }, .. }) if name == "write_file"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert_eq!(returned(run), json!("failed"));
}

#[test]
fn m4_for_in_member_and_for_of_destructuring_bindings() {
    let source = "function main(){let target={};for(target.key in {a:1,b:2}){}let out=[];for(const [key,value] of Object.entries({x:3,y:4})){out.push(key+value);}return [target.key,out];}";
    let run = runtime(source);
    let run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(returned(run), json!(["b",["x3","y4"]]));
    assert_eq!(returned(runtime("function main(){const items={x:1};try{for(let items in items){}}catch(e){return e.name;}}")), json!("ReferenceError"));
}

#[test]
fn source_variables_remain_visible_until_their_function_returns() {
    let source = "function main(){const doc=io.read({path:'large.txt'});const count=measure(doc.content);io.write({path:'count.txt',content:`${count}`});return count;} function measure(text){return text.length;}";
    let mut runtime = start(source, vec![]);
    let State::Action(_read) = runtime.next() else {
        panic!("read required")
    };
    let payload = "DEAD_INPUT_".repeat(6000);
    runtime.resume(json!({"content":payload}), false).unwrap();
    let State::Action(_write) = runtime.next() else {
        panic!("write required")
    };
    assert_eq!(runtime.named_bindings()["doc"]["content"], payload);
    let saved = serde_json::to_string(&runtime).unwrap();
    assert!(
        saved.contains("DEAD_INPUT_"),
        "source variables remain available to inherited AI scopes"
    );
    let mut restored: Runtime = serde_json::from_str(&saved).unwrap();
    restored.resume(json!({"saved":true}), false).unwrap();
    assert!(matches!(restored.next(),State::Returned(value) if value==payload.chars().count()));
}




#[test]
fn await_preserves_values_and_catchable_tool_failures() {
    assert_eq!(
        returned(runtime(
            "async function main(){const n=await 3; return n+1;}"
        )),
        json!(4)
    );
    let mut r = runtime(
        "async function main(){try {await io.read({path:'missing'}); return 'unexpected';} catch(e) {return e.message;}} ",
    );
    assert!(matches!(r.next(), State::Action(_)));
    r.resume(json!({"message":"read failed"}), true).unwrap();
    assert_eq!(returned(r), json!("read failed"));
}


#[test]
fn virtual_literal_throw_is_a_placeholder_but_other_effects_are_rejected() {
    let source = r#"
import { write } from "host:io";
const result = await inspect("data");
await write({path:"result.txt",content:result.text});
/** @virtual Review the provided data and return {text:string}. */
async function inspect(data) { throw new Error("Host must provide inspect"); }
"#;
    let mut run = top_level_with(source, vec![Value::Null]);
    assert!(
        matches!(run.next(), State::Action(a) if matches!(a.kind, ActionKind::Call { ref name, ref args, .. } if name == "inspect" && args[0] == "data"))
    );
    run.resume(json!({"text":"reviewed"}), false).unwrap();
    assert!(
        matches!(run.next(), State::Action(a) if matches!(a.kind, ActionKind::Tool { ref name, ref args, .. } if name == "write_file" && args["content"] == "reviewed"))
    );
    run.resume(json!({"ok":true}), false).unwrap();
    assert_eq!(returned(run), Value::Null);
    for body in [
        "throw new Error(io.read({path:'x'}));",
        "io.write({path:'x'}); throw new Error('x');",
        "throw 'oops';",
        "throw new TypeError('x');",
    ] {
        assert!(
            compile(&format!(
                "/** @module Review and return result. */ function inspect(){{{body}}} inspect();"
            ))
            .is_err(),
            "@module must reject {body}"
        );
        let virtual_program = compile(&format!(
            "/** @virtual Review and return result. */ function inspect(){{{body}}} inspect();"
        ))
        .unwrap_or_else(|error| panic!("@virtual partial body should compile {body}: {error:#}"));
        assert!(virtual_program.functions["inspect"].partial_virtual, "expected partial_virtual for {body}");
    }
    assert!(compile("/** @module Review. */ function inspect(){throw new Other('x');} inspect();").is_err());
}


#[test]
fn awaited_promise_chains_preserve_callback_values_and_tool_continuations() {
    assert_eq!(returned(runtime("async function main(){return await Promise.resolve(2).then(v=>v+=2);}")), json!(4));
    assert_eq!(returned(runtime("async function main(){return await Promise.resolve(1).then(x=>Promise.resolve(2).then(x=>x+1));}")), json!(3));
    assert_eq!(returned(runtime("async function main(){return await Promise.resolve(1).then(x=>Promise.resolve(2).then(y=>x+y));}")), json!(3));
    assert_eq!(
        returned(runtime(
            "async function main(){let x=10; const obj=await Promise.resolve(3).then(x=>({x})); return [x,obj.x];}"
        )),
        json!([10, 3])
    );
    assert_eq!(
        returned(runtime(
            "async function main(){return await Promise.reject({message:'bad'}).catch(e=>e.message);}"
        )),
        json!("bad")
    );
    let mut run = runtime(
        "async function main(){return await Promise.resolve('in').then(async p=>await io.read({path:p})).then(r=>r.content).catch(e=>e.message);}",
    );
    assert!(
        matches!(run.next(), State::Action(a) if matches!(a.kind, ActionKind::Tool { ref name, .. } if name == "read_file"))
    );
    let snapshot = serde_json::to_value(&run).unwrap();
    let mut restored: Runtime = serde_json::from_value(snapshot.clone()).unwrap();
    restored.resume(json!({"content":"hello"}), false).unwrap();
    assert_eq!(returned(restored), json!("hello"));
    let mut failed: Runtime = serde_json::from_value(snapshot).unwrap();
    failed
        .resume(json!({"message":"read failed"}), true)
        .unwrap();
    assert_eq!(returned(failed), json!("read failed"));
}

#[test]
fn m5_coroutine_survives_host_action_and_propagates_failure() {
    let mut run = runtime("async function main(){let order=[];async function work(){order.push('start');await 0;order.push('resume');throw new Error('bad');}let p=work();await io.read({path:'x'});order.push('caller');try{await p;}catch(e){order.push(e.message);}return order;}");
    assert!(matches!(run.next(), State::Action(_)));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"content":"ok"}), false).unwrap();
    assert_eq!(returned(run), json!(["start","resume","caller","bad"]));
}

#[test]
fn m5_async_returned_promises_are_adopted() {
    let source = "async function main(){async function a(){await 0;return 7;}async function b(){return a();}return await b();}";
    assert_eq!(returned(runtime(source)), json!(7));
}

#[test]
fn m5_unhandled_async_failure_is_reported() {
    let mut run = runtime("async function main(){async function work(){await 0;throw new Error('bad');}work();return 1;}");
    assert!(matches!(run.next(), State::Failed(e) if e["message"] == "bad"));
}

#[test]
fn m5_unhandled_async_failure_keeps_its_throw_site() {
    for source in [
        "async function main(){\nasync function work(){\nawait io.read({path:'x'});\nthrow new Error('bad');\n}\nwork().then(x=>x);\nreturn 1;\n}",
        "async function main(){\nasync function work(){\nawait io.read({path:'x'});\nthrow 'plain';\n}\nwork();\nreturn 1;\n}",
    ] {
        let mut run = runtime(source);
        assert!(matches!(run.next(), State::Action(_)));
        let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(json!({}), false).unwrap();
        let State::Failed(error) = run.next() else { panic!("expected failure") };
        assert_eq!(error["location"]["line"], 4, "{error}");
        assert!(error["stack"].as_array().is_some_and(|stack| stack.iter().any(|frame| frame["function"].as_str().is_some_and(|name| name.contains("work")))), "{error}");
    }
}

#[test]
fn m5_resumed_coroutine_keeps_caller_scope_through_host_suspension() {
    let mut run = runtime("async function main(){let secret='caller';async function work(){await 0;await io.read({path:'x'});return secret;}return await work();}");
    assert!(matches!(run.next(), State::Action(_)));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(run.named_bindings().get("secret"), Some(&json!("caller")));
    run.resume(json!({}), false).unwrap();
    assert_eq!(returned(run), json!("caller"));
}

#[test]
fn m5_coroutine_jobs_are_fifo_and_survive_gc() {
    let source = "async function main(){let order=[];async function work(n){await 0;order.push(n);}let a=work(1),b=work(2);for(let i=0;i<300;i++){}await a;await b;return order;}";
    assert_eq!(returned(runtime(source)), json!([1,2]));
}

#[test]
fn m5_promise_reactions_are_jobs_and_propagate_values_and_errors() {
    let source = r#"async function main(){
      let order=[]; let p=Promise.resolve(2);
      let q=p.then(x=>{order.push('then');return x+1;});
      order.push('caller');
      let a=await q;
      let b=await Promise.reject('bad').then(null, e=>e+'!');
      let c=await Promise.resolve(5).then(null);
      let d=await Promise.resolve(1).then(()=>{throw new Error('oops');}).catch(e=>e.message);
      return [order,a,b,c,d,Promise.resolve(p)===p];
    }"#;
    assert_eq!(returned(runtime(source)), json!([["caller","then"],3,"bad!",5,"oops",true]));
}

#[test]
fn m5_promise_callback_and_queue_survive_host_receipt_and_gc() {
    let source = r#"async function main(){
      let order=[]; let p=Promise.resolve(1).then(async x=>{await 0;order.push(x);return await io.read({path:'in'});});
      let q=Promise.resolve(2).then(x=>order.push(x));
      for(let i=0;i<300;i++){}
      let value=await p; await q; return [order,value.content];
    }"#;
    let mut run = runtime(source);
    assert!(matches!(run.next(), State::Action(a) if matches!(a.kind, ActionKind::Tool { ref name, .. } if name == "read_file")));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"content":"ok"}), false).unwrap();
    assert_eq!(returned(run), json!([[2,1],"ok"]));
}

#[test]
fn m5_promise_rejection_is_reported_only_if_unhandled() {
    assert_eq!(returned(runtime("async function main(){let p=Promise.reject('bad');let q=p.catch(e=>e);return await q;}")), json!("bad"));
    let mut run = runtime("async function main(){Promise.reject('bad').then();return 1;}");
    assert!(matches!(run.next(), State::Failed(e) if e["message"] == "bad"));
}

#[test]
fn m5_queued_promise_callback_survives_caller_host_suspension() {
    let source = "async function main(){let n=3;let p=Promise.resolve(2).then(function recur(x){if(x<=0)return n;return recur(x-1)+1;});for(let i=0;i<300;i++){}await io.read({path:'in'});return await p;}";
    let mut run = runtime(source);
    assert!(matches!(run.next(), State::Action(_)));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert_eq!(returned(run), json!(5));
}

#[test]
fn m5_promise_reaction_cannot_resolve_to_itself() {
    let source = "async function main(){let q;q=Promise.resolve().then(()=>q);try{await q;}catch(e){return e.message;}return 'unexpected';}";
    assert_eq!(returned(runtime(source)), json!("a promise cannot resolve to itself"));
}

#[test]
fn m5_promise_constructor_runs_synchronously_and_uses_first_settlement() {
    let source = r#"async function main(){let order=[];
      let p=new Promise((resolve,reject)=>{order.push('executor');resolve(4);reject('ignored');throw new Error('ignored');});
      order.push('caller');let value=await p;
      let failed=await new Promise(()=>{throw new Error('fail');}).catch(e=>e.message);
      let invalid;try{new Promise(7);}catch(e){invalid=e.name;}
      return [order,value,failed,invalid];}"#;
    assert_eq!(returned(runtime(source)), json!([["executor","caller"],4,"fail","TypeError"]));
}

#[test]
fn m5_promise_constructor_resolvers_survive_queues_and_adopt_pending_promises() {
    let source = r#"async function main(){let finish;
      const inner=new Promise(resolve=>{finish=resolve;});
      const outer=new Promise((resolve,reject)=>{resolve(inner);reject('ignored');});
      for(let i=0;i<300;i++){}
      Promise.resolve(8).then(finish);
      return await outer;
    }"#;
    assert_eq!(returned(runtime(source)), json!(8));
}

#[test]
fn m5_promise_executor_host_action_survives_snapshot_and_failure_is_catchable() {
    let source = "async function main(){let p=new Promise((resolve,reject)=>{let r=io.read({path:'in'});resolve(r.content);});try{return await p;}catch(e){return e.message;}}";
    let mut run = runtime(source);
    assert!(matches!(run.next(), State::Action(_)));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"message":"read failed"}), true).unwrap();
    assert_eq!(returned(run), json!("read failed"));
    let mut run = runtime(source);
    assert!(matches!(run.next(), State::Action(_)));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"content":"ok"}), false).unwrap();
    assert_eq!(returned(run), json!("ok"));
}

#[test]
fn m5_promise_executor_async_and_self_resolution() {
    let source = "async function main(){let p=new Promise(async resolve=>{await 0;resolve(3);});let n=await p;let finish;let q=new Promise(resolve=>{finish=resolve;});finish(q);try{await q;}catch(e){return [n,e.name];}return 'unexpected';}";
    assert_eq!(returned(runtime(source)), json!([3,"TypeError"]));
}

#[test]
fn m5_promise_resolvers_are_functions_and_json_ignores_them() {
    let source = "async function main(){let props;let p=new Promise(resolve=>{props=[typeof resolve,resolve instanceof Function,JSON.stringify([resolve]),JSON.stringify({resolve}),resolve.name,resolve.length];resolve();});await p;return props;}";
    assert_eq!(returned(runtime(source)), json!(["function",true,"[null]","{}","",1]));
}

#[test]
fn m5_general_promise_combinators_match_javascript() {
    let source = r#"async function main(){
      let all=await Promise.all([Promise.resolve(1),2,Promise.resolve(3)]);
      let settled=await Promise.allSettled([Promise.resolve('a'),Promise.reject('b')]);
      let race=await Promise.race([Promise.resolve('first'),Promise.resolve('second')]);
      let any=await Promise.any([Promise.reject('x'),Promise.resolve('ok')]);
      let failed;try{await Promise.any([Promise.reject('a'),Promise.reject('b')]);}catch(e){failed=[e.name,e.errors];}
      let empty=await Promise.all([]);
      return [all,settled,race,any,failed,empty];
    }"#;
    assert_eq!(returned(runtime(source)),json!([[1,2,3],[{"status":"fulfilled","value":"a"},{"status":"rejected","reason":"b"}],"first","ok",["AggregateError",["a","b"]],[]]));
}

#[test]
fn m5_async_array_map_and_promise_finally() {
    let source = r#"async function main(){let order=[];
      let promises=[1,2].map(async x=>{order.push(x);await 0;return x*2;});
      order.push('caller');let values=await Promise.all(promises);
      let result=await Promise.resolve(9).finally(()=>{order.push('finally');});
      let fail;try{await Promise.reject('bad').finally(()=>Promise.resolve());}catch(e){fail=e;}
      return [order,values,result,fail];}"#;
    assert_eq!(returned(runtime(source)),json!([[1,2,"caller","finally"],[2,4],9,"bad"]));
}

#[test]
fn m5_async_host_operations_are_queued_before_serial_dispatch_and_restore() {
    let source = r#"import {read} from 'host:io/promises';
async function main(){let one=read({path:'one'});let two=read({path:'two'});let a=await Promise.all([one,two]);return [a[0].content,a[1].content];}"#;
    let mut run = runtime(source);
    let State::Action(first) = run.next() else { panic!("expected read batch") };
    assert!(matches!(first.kind,ActionKind::ParallelReads{ref operations} if operations.len() == 2 && operations[0].1["path"] == "one" && operations[1].1["path"] == "two"));
    let snapshot = serde_json::to_value(&run).unwrap();
    assert_eq!(snapshot["active_async_batch"].as_array().unwrap().len(), 2);
    let mut run: Runtime = serde_json::from_value(snapshot).unwrap();
    run.resume(json!([{"failed":false,"value":{"content":"A"}},{"failed":false,"value":{"content":"B"}}]), false).unwrap();
    assert_eq!(returned(run),json!(["A","B"]));
}

#[test]
fn m5_async_virtual_calls_preserve_invocation_scope_and_reject_independently() {
    let source = r#"async function main(){let shared='first';let p=work('a');shared='second';let q=work('b');let a=await p;try{await q;}catch(e){return [a,e.message];}}
/** @virtual Perform work and return a string. */ async function work(request){}"#;
    let mut run = start_task(compile(source).unwrap(), vec![]);
    let State::Action(first) = run.next() else { panic!("expected first virtual call") };
    assert!(matches!(first.kind,ActionKind::Call{ref args,..} if args == &vec![json!("a")]));
    assert_eq!(run.named_bindings().get("shared"),Some(&json!("first")));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!("A"),false).unwrap();
    let State::Action(second) = run.next() else { panic!("expected second virtual call") };
    assert!(matches!(second.kind,ActionKind::Call{ref args,..} if args == &vec![json!("b")]));
    assert_eq!(run.named_bindings().get("shared"),Some(&json!("second")));
    run.resume(json!({"message":"bad"}),true).unwrap();
    assert_eq!(returned(run),json!(["A","bad"]));
}

#[test]
fn m5_async_read_batch_preserves_individual_failures_and_receipts() {
    let source = "import {read} from 'host:io/promises'; async function main(){let a=read({path:'missing'}),b=read({path:'ok'});let r=await Promise.allSettled([a,b]);return [r[0].status,r[0].reason.message,r[1].status,r[1].value.content];}";
    let mut run = runtime(source);
    let State::Action(action) = run.next() else { panic!("expected batch") };
    assert!(matches!(action.kind,ActionKind::ParallelReads{ref operations} if operations.len()==2));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert!(run.resume(json!([{"failed":false,"value":{}}]),false).is_err());
    assert!(matches!(run.next(),State::Action(a) if a.id == action.id));
    run.resume(json!([{"failed":true,"value":{"message":"not found"}},{"failed":false,"value":{"content":"ok"}}]),false).unwrap();
    assert_eq!(returned(run),json!(["rejected","not found","fulfilled","ok"]));
}

#[test]
fn m5_synchronous_host_action_blocks_queued_async_actions() {
    let source = "import {read as asyncRead} from 'host:io/promises'; import {read} from 'host:io'; async function main(){let a=asyncRead({path:'async'});let s=read({path:'sync'});let x=await a;return [s.content,x.content];}";
    let mut run = runtime(source);
    assert!(matches!(run.next(),State::Action(a) if matches!(a.kind,ActionKind::Tool{ref args,..} if args["path"]=="sync")));
    run.resume(json!({"content":"S"}),false).unwrap();
    assert!(matches!(run.next(),State::Action(a) if matches!(a.kind,ActionKind::Tool{ref args,..} if args["path"]=="async")));
    run.resume(json!({"content":"A"}),false).unwrap();
    assert_eq!(returned(run),json!(["S","A"]));
}

#[test]
fn synchronous_host_and_virtual_calls_resume_in_order_echo_results_and_stop_on_error() {
    let source = r#"
import * as io from "host:io";
const first = io.read({path:"first"});
console.log(first.content);
const verdict = check(first.content);
console.log(verdict.ok);
if (!verdict.ok) throw new Error("stop after verdict");
io.write({path:"must-not-run", content:"unexpected"});

/** @virtual Check the supplied value and return {ok:boolean}. */
function check(value) {}
"#;
    let mut run = top_level(source);
    let State::Action(first) = run.next() else { panic!("the first statement must ask for the first read") };
    assert!(matches!(first.kind, ActionKind::Tool { ref name, ref args }
        if name == "read_file" && args["path"] == "first"));
    run.resume(json!({"content":"observed"}), false).unwrap();

    let State::Action(check) = run.next() else { panic!("the next statement must call the virtual capability") };
    assert!(matches!(check.kind, ActionKind::Call { ref name, ref args, .. }
        if name == "check" && args == &vec![json!("observed")]));
    assert_eq!(run.logs(), &["observed"], "the completed host result is visible before the next statement");
    run.resume(json!({"ok":false}), false).unwrap();
    assert!(matches!(run.next(), State::Failed(error) if error["message"] == "stop after verdict"));
    assert_eq!(run.logs(), &["observed", "false"], "the virtual result is echoed before the branch");
}

#[test]
fn m6_concurrent_calls_resume_by_id_in_completion_order_after_snapshot() {
    use runtime::ConcurrentState;
    let source = r#"async function main(){let shared='first';let a=work('a');shared='second';let b=work('b');let first=await a;let c=work(first);return await Promise.all([b,c]);}
/** @virtual Perform work and return a string. */ async function work(request){}"#;
    let mut run = start(source, vec![]);
    let ConcurrentState::Actions(actions) = run.next_concurrent() else { panic!("expected async calls") };
    assert_eq!(actions.len(),2);
    let first = actions[0].id.clone(); let second = actions[1].id.clone();
    assert_eq!(run.async_scope(&first).unwrap().get("shared"),Some(&json!("first")));
    assert_eq!(run.async_scope(&second).unwrap().get("shared"),Some(&json!("second")));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(run.inflight_actions().len(),2);
    run.resume_action(&first,json!("A"),false).unwrap();
    assert!(run.resume_action(&first,json!("duplicate"),false).is_err());
    let ConcurrentState::Actions(next) = run.next_concurrent() else { panic!("expected third call") };
    assert_eq!(next.len(),1);
    assert!(matches!(&next[0].kind,ActionKind::Call{args,..} if args == &vec![json!("A")]));
    let third = next[0].id.clone();
    run.resume_action(&third,json!("C"),false).unwrap();
    assert!(matches!(run.next_concurrent(),ConcurrentState::Waiting));
    run.resume_action(&second,json!("B"),false).unwrap();
    assert!(matches!(run.next_concurrent(),ConcurrentState::Returned(v) if v == json!(["B","C"])));
}

#[test]
fn m6_async_virtual_launches_before_later_synchronous_effect() {
    use runtime::ConcurrentState;
    let source = r#"async function main(){let p=work('x');let data=io.read({path:'later'});return [data.content,await p];}
/** @virtual Perform work and return a string. */ async function work(request){}"#;
    let mut run = start_task(compile(source).unwrap(), vec![]);
    let ConcurrentState::Actions(calls) = run.next_concurrent() else { panic!("expected async call") };
    assert!(matches!(calls[0].kind,ActionKind::Call{..}));
    let ConcurrentState::Actions(tools) = run.next_concurrent() else { panic!("expected synchronous tool") };
    assert!(matches!(tools[0].kind,ActionKind::Tool{ref args,..} if args["path"]=="later"));
    run.resume_action(&tools[0].id,json!({"content":"read"}),false).unwrap();
    run.resume_action(&calls[0].id,json!("done"),false).unwrap();
    assert!(matches!(run.next_concurrent(),ConcurrentState::Returned(value) if value==json!(["read","done"])));
}

#[test]
fn m5_promise_then_accepts_host_tool_values_and_recovers() {
    let source="async function main(){let p=Promise.resolve({path:'x'}).then(io.read);return (await p).content;}";
    let mut run=runtime(source);
    assert!(matches!(run.next(),State::Action(a) if matches!(a.kind,ActionKind::Tool{ref name,..} if name=="read_file")));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"content":"ok"}),false).unwrap();
    assert_eq!(returned(run),json!("ok"));
}

#[test]
fn m7_fork_and_join_are_rejected_before_execution() {
    assert_eq!(returned(runtime("function main(){const join=(x)=>x+1;return join(4);}")),json!(5));
    for source in ["function main(){return fork(work);} function work(){return 1;}","function main(){return join('x');}"] {
        assert!(compile(source).unwrap_err().to_string().contains("fork/join have been retired"));
    }
}

#[test]
fn m6_virtual_calls_launch_even_when_an_earlier_async_tool_is_pending() {
    use runtime::ConcurrentState;
    let source=r#"import {read} from 'host:io/promises';async function main(){let a=read({path:'x'});let b=work('x');return [await a,await b];}
/** @virtual Work and return a value. */ async function work(item){}"#;
    let mut run=start_task(compile(source).unwrap(), vec![]);
    let ConcurrentState::Actions(calls)=run.next_concurrent() else {panic!("expected virtual call")};
    assert!(matches!(calls[0].kind,ActionKind::Call{..}));
    let ConcurrentState::Actions(tools)=run.next_concurrent() else {panic!("expected tool call")};
    assert!(matches!(tools[0].kind,ActionKind::Tool{..}));
    run.resume_action(&tools[0].id,json!({"content":"ok"}),false).unwrap();
    run.resume_action(&calls[0].id,json!("done"),false).unwrap();
    assert!(matches!(run.next_concurrent(),ConcurrentState::Returned(v) if v==json!([{"content":"ok"},"done"])));
}

#[test]
fn m5_promise_adoption_takes_javascript_microtask_turns() {
    // Both expected orders were recorded from Node 22 running the same source.
    let source = r#"async function main(){
      const log=[];
      async function f(){ return Promise.resolve('f'); }
      const done=[f().then(v=>log.push(v))];
      done.push(new Promise(r=>r(Promise.resolve('n'))).then(v=>log.push(v)));
      done.push(Promise.resolve().then(()=>Promise.resolve('t')).then(v=>log.push(v)));
      done.push(Promise.resolve().then(()=>log.push(1)).then(()=>log.push(2)).then(()=>log.push(3)).then(()=>log.push(4)).then(()=>log.push(5)));
      async function g(){ return 'g'; }
      done.push(g().then(v=>log.push(v)));
      await Promise.all(done);
      return log;
    }"#;
    assert_eq!(returned(runtime(source)), json!([1,"g",2,"f","n",3,"t",4,5]));
    // Adoption after a host suspension, with the runtime saved and restored.
    let mut run = runtime("async function main(){async function f(){await io.read({path:'x'});return Promise.resolve('late');}const log=[];const p=f().then(v=>log.push(v));Promise.resolve().then(()=>log.push(1)).then(()=>log.push(2)).then(()=>log.push(3));await p;return log;}");
    assert!(matches!(run.next(), State::Action(_)));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert_eq!(returned(run), json!([1,2,3,"late"]));
}

#[test]
fn m5_unawaited_async_calls_are_rejected_where_used_as_values() {
    let header = "import * as fs from \"host:io/promises\";\nimport {read as readAsync} from \"host:io/promises\";\n/** @virtual Inspect a page and return {passed:boolean}. */\nasync function inspect(page){}\nasync function work(){return 1;}\n";
    for fragment in [
        "const passed = inspect('a').passed;",
        "const text = readAsync({path:'a'}).content;",
        "const text = fs.read({path:'a'}).content;",
        "const text = fs.read({path:'a'})?.content;",
        "const text = (readAsync({path:'a'})).content.trim();",
        "const text = readAsync({path:'a'}).toString();",
        "if (inspect('a')) {}",
        "assert(inspect('a'), 'failed');",
        "const ok = !inspect('a');",
        "const ok = inspect('a') && true;",
        "const n = work() + 1;",
        "const ok = inspect('a') ? 1 : 2;",
        "while (work()) { break; }",
        "const s = `${work()}`;",
    ] {
        let source = format!("{header}async function main(){{\n{fragment}\n}}");
        let error = compile(&source).expect_err(fragment).to_string();
        assert!(error.contains("returns a Promise; await it"), "{fragment}: {error}");
    }
    for fragment in [
        "const passed = (await inspect('a')).passed;",
        "const text = (await readAsync({path:'a'})).content;",
        "const p = inspect('a'); const r = await p; return r.passed;",
        "return await Promise.all([inspect('a'), fs.read({path:'a'})]);",
        "return inspect('a').then(r => r.passed).catch(e => false).finally(() => {});",
        "const inspect = (page) => ({passed: true}); return inspect('a').passed;",
        "void work(); return 1;",
    ] {
        let source = format!("{header}async function main(){{\n{fragment}\n}}");
        compile(&source).unwrap_or_else(|error| panic!("{fragment}: {error:#}"));
    }
    // Top-level flows have no entry function around them.
    let error = compile(&format!("{header}const passed = inspect('a').passed;")).unwrap_err().to_string();
    assert!(error.contains("returns a Promise; await it"), "{error}");
    compile(&format!("{header}const passed = (await inspect('a')).passed;")).unwrap();
}


#[test]
fn a_comment_separated_by_a_blank_line_is_not_part_of_the_next_function_contract() {
    let program = compile("const draft = compose(1);\n// The flow ends after the checks.\n\n/** @virtual */\nfunction compose(request) {\n  // Write the page. Returns {content:string}.\n}\n").unwrap();
    let description = &program.functions["compose"].description;
    assert!(description.contains("Write the page"));
    assert!(!description.contains("The flow ends"), "{description}");
}

#[test]
fn a_nested_virtual_helper_is_ai_work_and_does_not_mark_its_enclosing_function() {
    let program = crate::runtime::agent_program::compiler::compile_body_with_scope(
        "async function designPage(shared, page) {\n  /** @virtual */\n  async function generate(page) {\n    // Write the page. Returns {content:string}.\n  }\n  return await generate(page);\n}\n",
        "designPage", crate::runtime::agent_program::libraries::builtin(), Default::default()).unwrap();
    assert!(!program.functions["designPage"].virtual_task);
    let (id, nested) = program.functions.iter().find(|(id, _)| id.starts_with("designPage/generate@")).unwrap();
    assert!(nested.virtual_task, "{id}");
    assert!(nested.description.contains("Write the page"));
    assert_eq!(nested.leaf_tools, Some(vec![]), "declared in an implementation: a leaf");
    let partial = crate::runtime::agent_program::compiler::compile_body_with_scope(
        "function f() {\n  /** @virtual */\n  function g() {\n    // Do it.\n    return 1;\n  }\n  return g();\n}\n",
        "f", crate::runtime::agent_program::libraries::builtin(), Default::default()).unwrap();
    let (_, nested_g) = partial.functions.iter().find(|(id, _)| id.starts_with("f/g@")).unwrap();
    assert!(nested_g.partial_virtual, "executable @virtual compiles as partial");
    assert_eq!(returned(Runtime::task_program(partial, "f", vec![]).unwrap()), json!(1));
}

#[test]
fn counted_repetition_over_any_character_compiles_as_in_javascript() {
    // Written by the AI in a real run: rejected under the former 1 MB budget.
    let source = "const text = \"可核验的证据\" + \"中间\".repeat(500) + \"不能单独作为证据\";\nassert(/(?:真实的完成证据|可核验的证据)[\\s\\S]{0,1800}(?:不能单独作为证据|不能单独)/.test(text), \"found within 1800\");\nassert(!/(?:真实的完成证据|可核验的证据)[\\s\\S]{0,10}(?:不能单独作为证据|不能单独)/.test(text), \"not within 10\");";
    let mut run = Runtime::task_program(compile(source).unwrap(), crate::runtime::agent_program::ENTRY, vec![]).unwrap();
    assert!(matches!(run.next(), State::Returned(_)), "both assertions hold");
}

#[test]
fn a_virtual_closes_over_the_scope_it_is_declared_in() {
    // Lexical, as in JavaScript: a local of the same name at the call site
    // does not shadow the module variable the declaration closes over.
    let source = r#"
const brief = "THE BRIEF";
function caller() { const brief = "LOCAL"; return judge("x"); }
const result = caller();
/**
 * @virtual
 * Judge x against the user's brief.
 * @uses brief what the user asked for
 * Returns {ok:boolean}.
 */
function judge(x) {}
"#;
    let mut run = top_level(source);
    let State::Action(action) = run.next() else { panic!("expected the virtual call") };
    let ActionKind::Call { name, args, captured } = action.kind else { panic!("expected a call") };
    assert_eq!((name.as_str(), args), ("judge", vec![json!("x")]));
    assert_eq!(captured, BTreeMap::from([("brief".to_owned(), json!("THE BRIEF"))]));
}

#[test]
fn an_async_virtual_reads_its_closure_when_it_is_called() {
    let source = r#"
let note = "BEFORE";
const pending = judge("x");
note = "AFTER";
await pending;
/**
 * @virtual
 * @uses note
 * Returns a string.
 */
async function judge(x) {}
"#;
    let mut run = top_level(source);
    let crate::runtime::agent_program::runtime::ConcurrentState::Actions(actions) = run.next_concurrent() else { panic!("expected the async call") };
    let ActionKind::Call { captured, .. } = &actions[0].kind else { panic!("expected a call") };
    assert_eq!(captured["note"], json!("BEFORE"), "read when called, not when the host runs it");
}

#[test]
fn uses_must_name_a_variable_in_scope_that_is_not_a_parameter() {
    let missing = compile("/**\n * @virtual\n * @uses missing\n * Returns a string.\n */\nfunction judge(x) {}\nconst r = judge(1);\n").unwrap_err();
    assert!(format!("{missing:#}").contains("@uses missing: no variable missing is in scope"), "{missing:#}");
    let parameter = compile("const x = 1;\n/**\n * @virtual\n * @uses x\n * Returns a string.\n */\nfunction judge(x) {}\nconst r = judge(1);\n").unwrap_err();
    assert!(format!("{parameter:#}").contains("@uses x: x is a parameter"), "{parameter:#}");
}

#[test]
fn a_nested_virtual_closes_over_its_enclosing_function() {
    let source = r#"
function outer() {
  const rules = "RULES";
  return inner("x");
  /**
   * @virtual
   * @uses rules
   * Returns a string.
   */
  function inner(x) {}
}
const r = outer();
"#;
    let mut run = top_level(source);
    let State::Action(action) = run.next() else { panic!("expected the virtual task") };
    let ActionKind::Call { args, captured, .. } = action.kind else { panic!("expected inner") };
    assert_eq!(args, vec![json!("x")]);
    assert_eq!(captured["rules"], json!("RULES"));
}

#[test]
fn a_variable_named_only_by_uses_stays_alive_until_the_call() {
    // Dead bindings are collected at each action; @uses is a reference too,
    // for a function's local closed over by a virtual declared inside it.
    let source = r#"
function outer() {
  const rules = "RULES";
  const first = draft("y");
  return inner(first);
  /**
   * @virtual
   * @uses rules
   * Returns a verdict.
   */
  function inner(x) {}
}
const result = outer();
/**
 * @virtual
 * Returns a draft.
 */
function draft(y) {}
"#;
    let mut run = top_level(source);
    let State::Action(first) = run.next() else { panic!("expected draft") };
    assert!(matches!(&first.kind, ActionKind::Call { name, .. } if name == "draft"));
    run.resume(json!("DRAFT"), false).unwrap();
    let state = run.next();
    let State::Action(second) = state else { panic!("expected inner: {state:?}") };
    let ActionKind::Call { captured, .. } = second.kind else { panic!("expected inner") };
    assert_eq!(captured["rules"], json!("RULES"));
}

#[test]
fn console_prints_lines_for_the_ai_that_wrote_the_program() {
    let source = r#"
const files = [{path: "a.txt"}, {path: "b.txt"}];
console.log("found", files.length, "files:", files);
console.warn("b.txt is empty");
const log = console.info;
log({done: true}, null, undefined, "end");
const result = console.error("x") === undefined;
"#;
    let mut run = top_level(source);
    while !matches!(run.next(), State::Returned(_) | State::Failed(_)) {}
    assert_eq!(run.logs(), [
        r#"found 2 files: [{"path":"a.txt"},{"path":"b.txt"}]"#,
        "[warn] b.txt is empty",
        r#"{"done":true} null undefined end"#,
        "[error] x",
    ]);
}

#[test]
fn console_retains_large_lines_without_a_character_cutoff() {
    let mut run = top_level("console.log('页'.repeat(99999)); console.log('x'.repeat(100001));");
    assert!(matches!(run.next(), State::Returned(_)));
    assert_eq!(run.logs()[0].chars().count(), 99_999);
    assert_eq!(run.logs()[1].chars().count(), 100_001);
    assert!(run.logs()[1].ends_with('x'));
}

#[test]
fn a_module_and_a_virtual_are_told_apart_by_their_declaration() {
    let program = compile("import * as io from \"host:io\";\nconst a = await summarize([\"x\"]);\nconst b = await judge(\"x\");\n\n/** @module */\nasync function summarize(paths) {\n  // Summarize the files. Returns {summary:string}.\n}\n\n/** @virtual */\nasync function judge(path) {\n  // Judge the file. Returns {keep:boolean}.\n}").unwrap();
    let summarize = &program.functions["summarize"];
    assert!(summarize.virtual_task && summarize.module_task && summarize.leaf_tools.is_none());
    assert_eq!(summarize.description, "Summarize the files. Returns {summary:string}.");
    let judge = &program.functions["judge"];
    assert!(judge.virtual_task && !judge.module_task);
    let tools = judge.leaf_tools.as_ref().expect("a @virtual in a main program is limited to its imports");
    assert!(tools.iter().any(|tool| tool == "read_file"), "{tools:?}");
    assert_eq!(judge.description, "Judge the file. Returns {keep:boolean}.");
}

#[test]
fn a_module_takes_parameters_not_uses() {
    let error = compile("const brief = \"x\";\nconst r = await summarize([]);\n\n/**\n * @module\n * Summarize the files as the brief asks.\n * @uses brief the user's request\n * Returns {summary:string}.\n */\nasync function summarize(paths) {}").unwrap_err().to_string();
    assert!(error.contains("@uses is for @virtual functions"), "{error}");
}

#[test]
fn a_function_is_either_virtual_or_a_module() {
    let error = compile("const r = await f(1);\n\n/** @virtual @module */\nasync function f(x) {\n  // Do x. Returns {ok:boolean}.\n}").unwrap_err().to_string();
    assert!(error.contains("both @virtual and @module"), "{error}");
}

#[test]
fn normal_function_can_chain_async_library_method_and_resume() {
    let mut run = runtime(r#"async function main(){
      const ppt=require('ppt/promises');
      function dump(){return ppt.get_design({page:3}).then(d=>d.slides+1);}
      return await dump();
    }"#);
    let State::Action(action) = run.next() else { panic!("require should link the library") };
    assert!(matches!(&action.kind, ActionKind::Require { library } if library=="ppt/promises"));
    run.resume(json!({"library":"ppt/promises","methods":{"get_design":"ppt_master__get_design"}}),false).unwrap();
    let State::Action(action) = run.next() else { panic!("async library should issue the host call") };
    assert!(matches!(&action.kind, ActionKind::Tool { name, .. } if name=="ppt_master__get_design"));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"slides":3}),false).unwrap();
    assert_eq!(returned(run),json!(4));
}

#[test]
fn synchronous_library_method_returns_plain_value_regardless_of_function_type() {
    let mut run = runtime(r#"async function main(){
      const ppt=require('ppt');
      function dump(){return ppt.get_design({page:3}).then(d=>d.slides);}
      try { return dump(); } catch (e) { return e.message; }
    }"#);
    assert!(matches!(run.next(), State::Action(_)));
    run.resume(json!({"library":"ppt","methods":{"get_design":"ppt_master__get_design"}}),false).unwrap();
    assert!(matches!(run.next(), State::Action(Action{kind:ActionKind::Tool{..},..})));
    run.resume(json!({"slides":3}),false).unwrap();
    let error = returned(run);
    assert!(error.as_str().is_some_and(|s|s.contains("then") && s.contains("not a function")),"{error}");
}

/// 2026-11-11: eighteen programs declared `async function main()` and never
/// called it, so each finished at once with nothing done.
#[test]
fn a_declared_function_nothing_calls_is_named_before_it_runs() {
    use super::uncalled_functions;
    let uncalled = uncalled_functions("async function main() { console.log(1); }", None).unwrap_err().to_string();
    assert!(uncalled.contains("TS_PROGRAM 1:16") && uncalled.contains("main") && uncalled.contains("await main();"), "{uncalled}");
    assert!(uncalled_functions("async function main() { console.log(1); }\nawait main();", None).is_ok());
    // Any function, nested ones included, not only main.
    let nested = uncalled_functions("function outer() { function helper() {} return 1; }\nconsole.log(outer());", None).unwrap_err().to_string();
    assert!(nested.contains("helper"), "{nested}");
    // Passed on as a value counts as used; a property named like it does not.
    assert!(uncalled_functions("function pick(x) { return x; }\nconsole.log([1].map(pick));", None).is_ok());
    assert!(uncalled_functions("function save() {}\nconst o = { save: 1 }; console.log(o.save);", None).is_err());
    // A module's entry is called by the host; its unused helper is not.
    assert!(uncalled_functions("async function summarize(paths) { return paths; }", Some("summarize")).is_ok());
    let helper = uncalled_functions("async function summarize(paths) { return paths; }\nfunction unused() {}", Some("summarize")).unwrap_err().to_string();
    assert!(helper.contains("unused") && helper.contains("only summarize"), "{helper}");
    assert!(uncalled_functions("export function api() {}", None).is_ok());
}

/// 2026-11-14: a main program called `collectBaseline`, a 20-line contract
/// whose JSDoc left out @virtual; as ordinary JS it returned undefined and the
/// main AI stopped orchestrating.
#[test]
fn a_contract_without_its_virtual_tag_is_named_before_it_runs() {
    use super::uncalled_functions;
    let untagged = "const found = await collectBaseline(\"log\");\nconsole.log(found);\n\n/**\n * Read the log and list failing tests.\n * Return {tests:[string]}.\n */\nasync function collectBaseline(path) {}";
    let error = uncalled_functions(untagged, None).unwrap_err().to_string();
    assert!(error.contains("TS_PROGRAM 8:1") && error.contains("collectBaseline") && error.contains("@virtual"), "{error}");
    // The same contract inside the body, as the examples write it.
    let inner = "console.log(judge(1));\nfunction judge(x) {\n  // Decide whether x is fine. Returns {ok:boolean}.\n}";
    assert!(uncalled_functions(inner, None).unwrap_err().to_string().contains("judge"));
    // Nested in a module implementation too.
    let nested = "async function run(x) {\n  /** Summarize x. Returns {text:string}. */\n  async function summarize(x) {}\n  return summarize(x);\n}";
    assert!(uncalled_functions(nested, Some("run")).unwrap_err().to_string().contains("summarize"));
    // Tagged contracts, in the header or the body, are AI work as declared.
    assert!(uncalled_functions(&untagged.replace(" * Read", " * @virtual\n * Read"), None).is_ok());
    assert!(uncalled_functions("console.log(judge(1));\n/** @module */\nfunction judge(x) {\n  // Returns {ok:boolean}.\n}", None).is_ok());
    assert!(uncalled_functions("console.log(judge(1));\nfunction judge(x) {\n  // @virtual Returns {ok:boolean}.\n}", None).is_ok());
    // An empty function with no contract, or a contract on a real body, is ordinary JS.
    assert!(uncalled_functions("function noop() {}\nnoop();", None).is_ok());
    assert!(uncalled_functions("/** Doubles x. */\nfunction twice(x) { return x * 2; }\nconsole.log(twice(1));", None).is_ok());
    // A comment separated by a blank line belongs to the code above it.
    assert!(uncalled_functions("const a = 1; // the first value\n\nfunction noop() {}\nnoop(); console.log(a);", None).is_ok());
}

#[test]
fn runtime_links_library_methods_and_supports_sync_and_promises() {
    use super::runtime::ConcurrentState;

    let program = crate::runtime::agent_program::compile(
        r#"
        const wf = require("doc-flow");
        const wfp = require("doc-flow/promises");
        const first = wf.summarize("a.txt", 2);
        const batch = await Promise.all([wfp.score("x"), wfp.score("y")]);
        console.log(JSON.stringify({ first, batch }));
        "#,
    )
    .unwrap();
    let mut run = Runtime::new(program, super::ENTRY, vec![]).unwrap();

    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Require { ref library }, .. }) if library == "doc-flow"));
    run.resume(json!({"library": "doc-flow", "methods": {}, "library_methods": ["summarize", "score"]}), false).unwrap();

    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Require { ref library }, .. }) if library == "doc-flow/promises"));
    run.resume(json!({"library": "doc-flow/promises", "methods": {}, "library_methods": ["summarize", "score"]}), false).unwrap();

    let State::Action(sync_call) = run.next() else { panic!("expected sync LibraryCall") };
    assert!(matches!(
        &sync_call.kind,
        ActionKind::LibraryCall { library, method, args }
            if library == "doc-flow" && method == "summarize" && args == &[json!("a.txt"), json!(2)]
    ));
    run.resume(json!({"done": true}), false).unwrap();

    let ConcurrentState::Actions(calls) = run.next_concurrent() else {
        panic!("expected concurrent Promise.all library calls")
    };
    assert_eq!(calls.len(), 2);
    for (call, item) in calls.iter().zip(["x", "y"]) {
        assert!(matches!(
            &call.kind,
            ActionKind::LibraryCall { library, method, args }
                if library == "doc-flow" && method == "score" && args == &[json!(item)]
        ));
    }
    let id0 = calls[0].id.clone();
    let id1 = calls[1].id.clone();
    run.resume_action(&id0, json!(10), false).unwrap();
    assert!(matches!(run.next_concurrent(), ConcurrentState::Waiting));
    run.resume_action(&id1, json!(20), false).unwrap();
    let ConcurrentState::Returned(_) = run.next_concurrent() else {
        panic!("expected Returned after settling both calls");
    };
    let output = run.logs().join("\n");
    let final_value: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(final_value, json!({"first": {"done": true}, "batch": [10, 20]}));
}

#[test]
fn partial_virtual_returns_early_without_ai_or_falls_through_capturing_locals_and_closure() {
    let source = r#"
const io = require("io");
const process = require("process");
const host = require("host");
const mainContext = host.context();

/** @virtual Review target using prepared subContext and return { mode, score }. */
async function analyze(target, fastHit) {
  const subContext = { ...mainContext, messages: mainContext.messages.slice(0, 1), tools: { io } };
  if (fastHit) {
    return { mode: "js-fast", target };
  }
  const suffix = target + ":fallback";
  // Hand off remaining analysis to AI with live subContext & suffix.
}

const fast = await analyze("a.ts", true);
const ai = await analyze("b.ts", false);
console.log(JSON.stringify({ fast, ai }));
"#;
    let program = compile(source).unwrap();
    assert!(program.functions["analyze"].partial_virtual);
    let mut run = Runtime::new(program, super::ENTRY, vec![]).unwrap();

    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Require { ref library }, .. }) if library == "io"));
    run.resume(json!({"library": "io", "methods": {"read": "read_file"}}), false).unwrap();
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Require { ref library }, .. }) if library == "process"));
    run.resume(json!({"library": "process", "methods": {"exec": "exec_command"}}), false).unwrap();
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Require { ref library }, .. }) if library == "host"));
    run.resume(json!({"library": "host", "methods": {}, "library_methods": ["context", "tools"]}), false).unwrap();

    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::LibraryCall { ref library, ref method, .. }, .. }) if library == "host" && method == "context"));
    run.resume(json!({
        "messages": [{"role": "system", "content": "sys"}, {"role": "user", "content": "heavy"}],
        "tools": ["read_file", "exec_command"],
        "prompt_cache_key": "task-key-1"
    }), false).unwrap();

    // First call (`fastHit = true`) returns directly in JS; next Action is the second call's Task!
    let State::Action(action) = run.next() else { panic!("expected second call ActionKind::Task") };
    assert!(matches!(&action.kind, ActionKind::Task { name, input, .. } if name == "analyze" && input["target"] == "b.ts"), "{action:?}");
    let scope = action.scope_variables().expect("captured scope");
    assert_eq!(scope["suffix"], "b.ts:fallback");
    assert_eq!(scope["subContext"]["messages"], json!([{"role": "system", "content": "sys"}]));
    assert_eq!(scope["mainContext"]["prompt_cache_key"], "task-key-1");
    assert_eq!(action.scope_libraries(), &["io".to_string()]);
    // Bare module requires (`io`, `process`, `host`) are omitted from scope JSON while `io` referenced inside `subContext.tools` is tracked in `scope_libraries`.
    assert!(!scope.contains_key("io"));
    assert!(!scope.contains_key("process"));

    run.resume(json!({"mode": "ai", "score": 9}), false).unwrap();
    let State::Returned(_) = run.next() else { panic!("expected Returned") };
    let out: Value = serde_json::from_str(run.logs().join("\n").trim()).unwrap();
    assert_eq!(out, json!({
        "fast": {"mode": "js-fast", "target": "a.ts"},
        "ai": {"mode": "ai", "score": 9}
    }));
}

#[test]
fn virtual_call_captures_lexical_closure_and_library_refs_without_uses() {
    let source = r#"
const io = require("io");
const process = require("process");
const subTools = { io };
const note = "keep-this-note";
const result = judge("item.txt");
console.log(JSON.stringify(result));

/** @virtual Judge item.txt using lexical note and subTools. */
function judge(item) {}
"#;
    let program = compile(source).unwrap();
    assert!(!program.functions["judge"].partial_virtual);
    assert!(program.functions["judge"].uses.is_empty());
    let mut run = Runtime::new(program, super::ENTRY, vec![]).unwrap();

    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Require { ref library }, .. }) if library == "io"));
    run.resume(json!({"library": "io", "methods": {"read": "read_file"}}), false).unwrap();
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Require { ref library }, .. }) if library == "process"));
    run.resume(json!({"library": "process", "methods": {"exec": "exec_command"}}), false).unwrap();

    let State::Action(action) = run.next() else { panic!("expected ActionKind::Call") };
    assert!(matches!(&action.kind, ActionKind::Call { name, args, captured } if name == "judge" && args == &[json!("item.txt")] && captured.is_empty()));
    let scope = action.scope_variables().expect("captured scope");
    assert_eq!(scope["note"], "keep-this-note");
    assert_eq!(action.scope_libraries(), &["io".to_string()]);
    assert!(!scope.contains_key("process"));
}
