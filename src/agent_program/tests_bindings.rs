use super::*;

#[test]
fn bindings_a_local_named_tools_is_an_ordinary_variable() {
    assert_eq!(returned(runtime(r#"function main(){
      const tools = [];
      tools.push('cargo');
      function more({tools}){ return tools.length; }
      return [tools, more({tools:[1,2]}), ((tools)=>tools*2)(4)];
    }"#)), json!([["cargo"],2,8]));
}

#[test]
fn bindings_module_implementation_may_name_a_local_tools() {
    let source = "async function surveyProject(input) {\n  const tools = surveyBuildTools([], new Map());\n  return { tools: tools };\n}\nfunction surveyBuildTools(files, texts) {\n  const tools = [];\n  tools.push('npm');\n  return tools;\n}";
    let program = compile_body(source, "surveyProject").unwrap();
    let run = Runtime::task_program(program, "surveyProject", vec![json!({})]).unwrap();
    assert_eq!(returned(run), json!({"tools":["npm"]}));
}

#[test]
fn bindings_helpers_may_be_named_tools_call_or_ai() {
    assert_eq!(returned(runtime("function main(){return tools()+call(2)+ai();}\nfunction tools(){return 1;}\nfunction call(n){return n;}\nfunction ai(){return 4;}")), json!(7));
}

#[test]
fn bindings_the_retired_global_tools_object_is_still_reported() {
    let error = compile("function main(){return tools.read_file({path:'a'});}").unwrap_err().to_string();
    assert!(error.contains("tools.* is unsupported"), "{error}");
}

#[test]
fn bindings_a_rejected_declaration_names_itself_at_its_own_position() {
    let error = compile("function main(){\n  const ok = 1;\n  const JSON = ok;\n  return JSON;\n}").unwrap_err().to_string();
    assert!(error.contains("TS_PROGRAM 3:9:") && error.contains("JSON is a host built-in"), "{error}");
    let error = compile_body("async function work(input) {\n  return helper();\n}\nfunction helper() {\n  const x = 1;\n  if (x) {\n    const assert = x;\n    return assert;\n  }\n}", "work").unwrap_err().to_string();
    assert!(error.contains("TS_PROGRAM 7:11:") && error.contains("assert is a host built-in"), "{error}");
}

#[test]
fn bindings_a_parameter_and_body_conflict_points_at_the_body_binding() {
    let error = compile("function main(){return f(1);}\nfunction f(a) {\n  let b = 1;\n  let a = 2;\n  return a + b;\n}").unwrap_err().to_string();
    assert!(error.contains("TS_PROGRAM 4:7:") && error.contains(" a"), "{error}");
}

const MODULE_WITH_TOP_LEVEL: &str = r#"const io = require("io");
const SKIP = new Set(["target", "node_modules"]);
let calls = 0;
const label = (name) => SKIP.has(name) ? "skipped" : name;

async function survey(input) {
  calls += 1;
  const text = io.read({ path: input.path });
  return { calls, text, kinds: input.names.map(label), checked: count() };
}
function count() { return calls; }"#;

#[test]
fn bindings_module_top_level_runs_before_its_entry_and_survives_restore() {
    let program = compile_body(MODULE_WITH_TOP_LEVEL, "survey").unwrap();
    let mut run = Runtime::task_program(program, "survey", vec![json!({"path":"a.txt","names":["src","target"]})]).unwrap();
    let State::Action(action) = run.next() else {panic!("the top-level require runs first")};
    assert!(matches!(action.kind, ActionKind::Require { .. }), "{action:?}");
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    run.resume(json!({"library":"io","methods":{"read":"read_file"}}), false).unwrap();
    let State::Action(action) = run.next() else {panic!("the entry calls the required library")};
    assert!(matches!(&action.kind, ActionKind::Tool { name, .. } if name == "read_file"), "{action:?}");
    run.resume(json!("hello"), false).unwrap();
    assert_eq!(returned(run), json!({"calls":1,"text":"hello","kinds":["src","skipped"],"checked":1}));
}

#[test]
fn bindings_each_module_call_initializes_its_own_top_level() {
    let source = "let calls = 0;\nfunction work(n) { calls += n; return calls; }";
    for _ in 0..2 {
        let program = compile_body(source, "work").unwrap();
        assert_eq!(returned(Runtime::task_program(program, "work", vec![json!(5)]).unwrap()), json!(5));
    }
}

#[test]
fn bindings_module_top_level_await_is_reported_where_it_is() {
    let error = compile_body("const ready = Promise.resolve(1);\nconst value = await ready;\nasync function work() { return value; }", "work").unwrap_err().to_string();
    assert!(error.contains("TS_PROGRAM 2:15:") && error.contains("put this await inside work"), "{error}");
    assert!(compile_body("const f = async () => { await 1; };\nasync function work() { return 1; }", "work").is_ok());
    // A parser error is explained, not just named.
    let error = compile_body("const x = 1;\nreturn x;\nfunction work() { return 1; }", "work").unwrap_err().to_string();
    assert!(error.contains("TS_PROGRAM 2:1:") && error.contains("put this code inside work") && !error.contains("ReturnNotAllowed"), "{error}");
}

/// The 2026-11-11 run: a module author wrote three probes as scripts (top-level
/// await, a final return, a helper awaited at the top level) and each was
/// rejected. A probe that never defines the called function runs as its body.
#[test]
fn bindings_a_probe_written_as_a_script_runs_as_the_called_function() {
    use super::compiler::probe_as_entry;
    let params = vec!["input".to_owned()];
    for script in [
        "const out = {};\nout.n = await Promise.resolve(input.n + 1);\nreturn out;",
        "async function probe() {\n  return {n: await Promise.resolve(input.n + 1)};\n}\nconst out = await probe();\nreturn out;",
    ] {
        let source = probe_as_entry(script, "inspect", &params).expect("a script is wrapped");
        assert_eq!(source.lines().count(), script.lines().count() + 1, "lines stay where they were: {source}");
        let program = compile_body(&source, "inspect").unwrap();
        assert_eq!(returned(Runtime::task_program(program, "inspect", vec![json!({"n":1})]).unwrap()), json!({"n":2}), "{script}");
    }
    // An import stays a module import, after the function, where it still hoists.
    let source = probe_as_entry("import * as io from \"host:io\";\nconst info = io.workspace_info({});\nreturn typeof io.read;", "inspect", &params).unwrap();
    assert!(source.ends_with("import * as io from \"host:io\";") && source.lines().nth(1).unwrap().contains("io.workspace_info"), "{source}");
    assert!(compile_body(&source, "inspect").is_ok());
    // Errors keep the line the author wrote.
    let source = probe_as_entry("const a = 1;\nconst b = a.c.d;\nreturn b;", "inspect", &params).unwrap();
    let program = compile_body(&source, "inspect").unwrap();
    let error = Runtime::task_program(program, "inspect", vec![json!({})]).unwrap().next();
    assert!(matches!(&error, State::Failed(e) if e.to_string().contains("\"line\":2")), "{error:?}");
    // A source that defines the called function, or only declares, is left as it is.
    assert!(probe_as_entry("async function inspect(input) { return 1; }", "inspect", &params).is_none());
    assert!(probe_as_entry("function helper() {}", "inspect", &params).is_none());
}

#[test]
fn bindings_module_top_level_errors_fail_the_call_at_their_line() {
    let program = compile_body("const config = {};\nconst limit = config.limits.max;\nfunction work() { return limit; }", "work").unwrap();
    let State::Failed(error) = Runtime::task_program(program, "work", vec![]).unwrap().next() else {panic!("initialization must fail")};
    assert_eq!(error["location"]["line"], 2, "{error}");
}

#[test]
fn bindings_module_top_level_sees_the_scope_the_host_passes_in() {
    let program = compile_body_with_scope("const offset = base + 1;\nfunction work(n) { return n + offset; }", "work", libraries::builtin(), ["base".to_owned()].into()).unwrap();
    let run = Runtime::task_program_with_scope(program, "work", vec![json!(5)], [("base".to_owned(), json!(10))].into()).unwrap();
    assert_eq!(returned(run), json!(16));
}

#[test]
fn commonjs_module_and_exports_are_ordinary_objects() {
    assert_eq!(returned(runtime("function main(){\n  module.exports.x = 1;\n  exports.y = 2;\n  return [module.exports, exports === module.exports, require.main === module, Boolean(require.main)];\n}")),
        json!([{"x":1,"y":2}, true, false, false]));
}

#[test]
fn commonjs_exports_in_a_module_implementation_do_not_change_its_entry() {
    for tail in ["module.exports = { work, helper };", "exports.work = work;", "module.exports = work;", "if (require.main === module) work();", "if (module === require.main) { work(); }"] {
        let source = format!("async function work(input) {{ return helper(); }}\nfunction helper() {{ return 3; }}\n{tail}");
        let program = compile_body(&source, "work").unwrap_or_else(|e| panic!("{tail}: {e:#}"));
        assert_eq!(returned(Runtime::task_program(program, "work", vec![json!({})]).unwrap()), json!(3), "{tail}");
    }
}

#[test]
fn commonjs_calling_the_entry_at_top_level_is_reported() {
    for (source, at) in [("function work() { return 1; }\nwork();", "2:1"), ("async function work() { return 1; }\nconst run = () => 0;\nwork().then(console.log);", "3:1")] {
        let error = compile_body(source, "work").unwrap_err().to_string();
        assert!(error.contains(&format!("TS_PROGRAM {at}:")) && error.contains("would run it twice"), "{error}");
    }
}

#[test]
fn commonjs_a_source_may_declare_its_own_module_or_exports() {
    let program = compile_body("const module = { name: 'own' };\nlet exports = [1];\nfunction work(){ return [module.name, exports]; }", "work").unwrap();
    assert!(!program.commonjs);
    assert_eq!(returned(Runtime::task_program(program, "work", vec![]).unwrap()), json!(["own",[1]]));
}

#[test]
fn commonjs_objects_survive_a_snapshot() {
    let mut run = runtime("function main(){\n  exports.before = 1;\n  const value = io.read({path:'a'});\n  module.exports.after = value;\n  return module.exports;\n}");
    assert!(matches!(run.next(), State::Action(_)));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    run.resume(json!("text"), false).unwrap();
    assert_eq!(returned(run), json!({"before":1,"after":"text"}));
}

