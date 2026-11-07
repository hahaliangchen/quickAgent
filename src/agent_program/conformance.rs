//! Opt-in test262 and model-source measurements. No model or host effects run.
//! Minimal assertion hooks exist only in the test binary, never production.
use super::heap::{Heap, HeapId, HeapNode, VmValue};
use super::*;
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const THROWS: &str = "function __test262Throws(expected,callback,message){\nlet caught=false;\ntry{callback();}catch(error){caught=true;__test262CheckThrown(expected,error,message);}\nif(!caught){throw new Test262Error('assert.throws: callback did not throw');}\n}\n";

pub(super) fn install(heap: &mut Heap, env: HeapId) -> Result<()> {
    heap.install_test_error()?;
    for name in ["sameValue", "notSameValue", "throws", "compareArray"] {
        heap.native(&format!("test262.assert.{name}"))?;
    }
    for (binding, native) in [
        ("assert", "test262.assert"),
        ("__test262CheckThrown", "test262.checkThrown"),
        ("$DONE", "test262.done"),
        ("compareArray", "test262.compareArray"),
    ] {
        let value = VmValue::Reference(heap.native(native)?);
        heap.define_binding(env, binding, value)?;
    }
    heap.define_binding(env, "__test262DoneCalled", VmValue::Boolean(false))?;
    Ok(())
}
fn same(a: &VmValue, b: &VmValue) -> bool {
    if let (VmValue::Number(a), VmValue::Number(b)) = (a, b) {
        let (a, b) = (a.get(), b.get());
        return (a.is_nan() && b.is_nan())
            || (a == b && (a != 0.0 || a.is_sign_negative() == b.is_sign_negative()));
    }
    super::js_value::strict_equal(a, b)
}
pub(super) fn builtin(
    heap: &mut Heap,
    name: &str,
    args: &[VmValue],
    env: Option<HeapId>,
) -> Result<VmValue> {
    let get = |n| args.get(n).cloned().unwrap_or(VmValue::Undefined);
    match name {
        "test262.error" => {
            return super::js_methods::new_error(heap, "Test262Error", &get(0), &get(1));
        }
        "test262.assert" => ensure!(
            get(0) == VmValue::Boolean(true),
            "Test262Error: assert expected true"
        ),
        "test262.assert.sameValue" => ensure!(
            same(&get(0), &get(1)),
            "Test262Error: assert.sameValue failed"
        ),
        "test262.assert.notSameValue" => ensure!(
            !same(&get(0), &get(1)),
            "Test262Error: assert.notSameValue failed"
        ),
        "test262.checkThrown" => {
            let expected=get(0);let actual=get(1);
            ensure!(super::vm_eval::callable(heap,&expected),"Test262Error: expected a constructor function");
            let harness_error=matches!(&expected,VmValue::Reference(id) if matches!(heap.get(*id),Ok(HeapNode::Native(name)) if name=="test262.error"));
            if harness_error {
                ensure!(matches!(&actual,VmValue::Reference(id) if heap.class_of(*id)==Some("Test262Error")),"Test262Error: assert.throws constructor mismatch");
            } else {
                ensure!(matches!(actual,VmValue::Reference(_)),"Test262Error: assert.throws received a primitive");
                let constructor=super::vm_eval::member(heap,&actual,&VmValue::String("constructor".into()))?;
                ensure!(super::js_value::strict_equal(&constructor,&expected),"Test262Error: assert.throws constructor mismatch");
            }
        }
        "test262.assert.compareArray" | "test262.compareArray" => {
            let (VmValue::Reference(a), VmValue::Reference(b)) = (get(0), get(1)) else {
                bail!("Test262Error: compareArray expects arrays")
            };
            let (HeapNode::Array(a), HeapNode::Array(b)) = (heap.get(a)?, heap.get(b)?) else {
                bail!("Test262Error: compareArray expects arrays")
            };
            let equal = a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b));
            if name == "test262.compareArray" {
                return Ok(VmValue::Boolean(equal));
            }
            ensure!(equal, "Test262Error: assert.compareArray failed");
        }
        "test262.done" => {
            let env = env.context("missing test realm")?;
            ensure!(
                heap.lookup_binding(env, "__test262DoneCalled")? != VmValue::Boolean(true),
                "Test262Error: $DONE called twice"
            );
            heap.assign_binding(env, "__test262DoneCalled", VmValue::Boolean(true))?;
            if get(0) != VmValue::Undefined {
                bail!("Test262Error: $DONE received an error");
            }
        }
        _ => bail!("Test262Error: unsupported harness hook {name}"),
    }
    Ok(VmValue::Undefined)
}

#[derive(Default, serde::Deserialize)]
struct Metadata {
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    includes: Vec<String>,
    negative: Option<Negative>,
}
#[derive(serde::Deserialize)]
struct Negative {
    phase: String,
    #[serde(rename = "type")]
    kind: String,
}
fn metadata(source: &str) -> Result<Metadata> {
    let Some((_, rest)) = source.split_once("/*---") else {
        return Ok(Metadata::default());
    };
    let text = rest
        .split_once("---*/")
        .context("unterminated test262 frontmatter")?
        .0;
    Ok(serde_yaml::from_str(text)?)
}
fn execute(source: &str, meta: &Metadata, includes: &str) -> (String, String) {
    if meta.flags.iter().any(|f| f == "noStrict") {
        return (
            "compile_rejected".into(),
            "noStrict script mode unavailable: VM currently compiles strict module input".into(),
        );
    }
    if let Some(n) = &meta.negative {
        if n.phase == "parse" || n.phase == "early" {
            let parse_source = if meta.flags.iter().any(|f| f == "onlyStrict") {
                format!("\"use strict\";\n{source}")
            } else {
                source.into()
            };
            return if n.kind == "SyntaxError" && super::compiler::script_parse_error(&parse_source)
            {
                (
                    "pass".into(),
                    "negative: parser rejected SyntaxError".into(),
                )
            } else {
                ("compile_rejected".into(),"negative parse/early test not rejected by the parser; semantic compiler rejection is not a SyntaxError pass".into())
            };
        }
    }
    let raw = meta.flags.iter().any(|f| f == "raw");
    let text = if raw {
        source.to_owned()
    } else {
        format!("{THROWS}{includes}\n{source}")
    };
    let compiled=if raw {super::compile_with_libraries(&text,super::libraries::Libraries::new())} else {super::compiler::compile_conformance(&text)};
    let program = match compiled {
        Ok(p) => p,
        Err(e) => return ("compile_rejected".into(), e.to_string()),
    };
    let initialized=if raw {Runtime::new(program,super::ENTRY,vec![])} else {Runtime::conformance(program)};
    let mut runtime = match initialized {
        Ok(r) => r,
        Err(e) => return ("run_failed".into(), e.to_string()),
    };
    match runtime.next() {
        State::Returned(_) => {
            if meta.negative.is_some() {
                (
                    "run_failed".into(),
                    "expected exception was not thrown".into(),
                )
            } else if meta.flags.iter().any(|f| f == "async") && !runtime.conformance_done() {
                (
                    "run_failed".into(),
                    "async test finished without $DONE".into(),
                )
            } else {
                ("pass".into(), String::new())
            }
        }
        State::Failed(e) => {
            if meta
                .negative
                .as_ref()
                .is_some_and(|n| n.phase == "runtime" && e["name"] == n.kind)
            {
                ("pass".into(), "negative: expected runtime exception".into())
            } else {
                ("run_failed".into(), e.to_string())
            }
        }
        other => (
            "run_failed".into(),
            format!("unexpected host action/wait (not executed): {other:?}"),
        ),
    }
}
fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut output = vec![];
    for e in std::fs::read_dir(root)? {
        let p = e?.path();
        let meta = std::fs::symlink_metadata(&p)?;
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            output.extend(files(&p)?);
        } else if p.extension().is_some_and(|x| x == "js") {
            output.push(p);
        }
    }
    output.sort();
    Ok(output)
}
fn hash(source: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(source.as_bytes()))
}
fn write_report(report: &Value, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(report)?)?;
    Ok(())
}
fn compare(report: &Value, path: &Path) -> Result<()> {
    let old: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        old["revision"] == report["revision"] && old["selection_hash"] == report["selection_hash"],
        "baseline revision/selection differ; do not compare different test populations"
    );
    let new: BTreeMap<_, _> = report["cases"]
        .as_array()
        .context("cases")?
        .iter()
        .map(|r| (r["id"].as_str().unwrap(), r))
        .collect();
    ensure!(
        new.len() == old["cases"].as_array().context("old cases")?.len(),
        "baseline population changed"
    );
    let mut regressions = vec![];
    for row in old["cases"].as_array().context("old cases")? {
        ensure!(
            new.get(row["id"].as_str().unwrap())
                .is_some_and(|r| r["source_hash"] == row["source_hash"]),
            "baseline source/identity changed: {}",
            row["id"]
        );
        if row["status"] == "pass"
            && new
                .get(row["id"].as_str().unwrap())
                .is_none_or(|r| r["status"] != "pass" || r["source_hash"] != row["source_hash"])
        {
            regressions.push(row["id"].clone());
        }
    }
    ensure!(
        regressions.is_empty(),
        "conformance regressions: {regressions:?}"
    );
    Ok(())
}

#[test]
fn vm_stage0_harness_distinguishes_failures_and_error_brands() {
    for source in [
        "assert.sameValue(NaN,NaN);assert.notSameValue(0,-0);",
        "assert.throws(Test262Error,()=>{throw new Test262Error('x');});",
        "assert.compareArray([1,2],[1,2]);assert(compareArray([0],[0]));",
    ] {
        assert_eq!(
            execute(source, &Metadata::default(), "").0,
            "pass",
            "{source}"
        );
    }
    for source in [
        "assert(1);",
        "assert.sameValue(0,-0);",
        "assert.throws(Test262Error,()=>{throw new RangeError('x');});",
        "assert.throws(Test262Error,()=>3);",
        "throw new Test262Error('fail');",
    ] {
        assert_eq!(
            execute(source, &Metadata::default(), "").0,
            "run_failed",
            "{source}"
        );
    }
}
#[test]
fn vm_stage0_harness_negative_tests_do_not_confuse_unsupported_with_syntax_errors() {
    // Error constructors are now real intrinsic values in production too.
    let result=execute(
            "assert.throws(TypeError,()=>{throw new TypeError('x');});",
            &Metadata::default(),
            ""
        );
    assert_eq!(result.0,"pass","{}",result.1);
    let result=execute("function Custom(){}assert.throws(Custom,()=>{throw new Custom();});",&Metadata::default(),"");
    assert_eq!(result.0,"pass","{}",result.1);
    let m = Metadata {
        negative: Some(Negative {
            phase: "parse".into(),
            kind: "SyntaxError".into(),
        }),
        ..Default::default()
    };
    assert_eq!(execute("let x=;", &m, "").0, "pass");
    assert_eq!(execute("new Map();", &m, "").0, "compile_rejected");
    let m = Metadata {
        negative: Some(Negative {
            phase: "runtime".into(),
            kind: "TypeError".into(),
        }),
        ..Default::default()
    };
    assert_eq!(execute("throw new TypeError('x');", &m, "").0, "pass");
    assert_eq!(
        execute("throw new RangeError('x');", &m, "").0,
        "run_failed"
    );
}
#[test]
fn vm_stage0_harness_async_completion_and_snapshot() {
    let m = Metadata {
        flags: vec!["async".into()],
        ..Default::default()
    };
    assert_eq!(
        execute(
            "Promise.resolve(1).then(x=>{assert.sameValue(x,1);$DONE();});",
            &m,
            ""
        )
        .0,
        "pass"
    );
    assert_eq!(execute("const n=1;", &m, "").0, "run_failed");
    let p = super::compiler::compile_conformance(&format!("{THROWS}assert.sameValue(NaN,NaN);"))
        .unwrap();
    let run = Runtime::conformance(p).unwrap();
    let mut restored: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert!(matches!(restored.next(), State::Returned(_)));
}
#[test]
fn vm_stage0_baseline_cannot_silently_drop_or_change_a_passing_case() {
    let path = std::env::temp_dir().join(format!(
        "vm-baseline-{}.json",
        crate::agent_service::uuid_like()
    ));
    let old = json!({"revision":"pin","selection_hash":"s","cases":[{"id":"a","source_hash":"1","status":"pass"}]});
    write_report(&old, &path).unwrap();
    assert!(compare(&old, &path).is_ok());
    let changed = json!({"revision":"pin","selection_hash":"s","cases":[]});
    assert!(compare(&changed, &path).is_err());
    std::fs::remove_file(path).unwrap();
}

#[test]
#[ignore = "set TEST262_ROOT and VM_TEST262_REPORT; no downloads or models are started"]
fn test262_subset() -> Result<()> {
    let root = PathBuf::from(std::env::var("TEST262_ROOT").context("TEST262_ROOT is required")?);
    let config_text=match std::env::var("VM_TEST262_SELECTION") {
        Ok(path)=>std::fs::read_to_string(path)?,
        Err(_)=>include_str!("../../tests/vm/test262-selection.json").to_owned(),
    };
    let config: Value = serde_json::from_str(&config_text)?;
    let allowed: BTreeSet<_> = config["features"]
        .as_array()
        .context("features")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let filter = std::env::var("VM_TEST262_FILTER")
        .ok()
        .map(|s| regex::Regex::new(&s))
        .transpose()?;
    let revision = config["revision"].as_str().context("revision")?;
    // Archive extracts need no .git. Content hashes below pin every measured file.
    ensure!(
        root.join("LICENSE").is_file() && root.join("harness/assert.js").is_file(),
        "not a test262 checkout"
    );
    let pin: Value = serde_json::from_slice(
        &std::fs::read(root.join(".vm-test262-pin.json"))
            .context("prepare this pinned checkout with scripts/vm/prepare-test262.py")?,
    )?;
    ensure!(
        pin["revision"] == config["revision"] && pin["archive_sha256"] == config["archive_sha256"],
        "test262 pin does not match selection"
    );
    let mut cases = vec![];
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut groups: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for directory in config["directories"].as_array().context("directories")? {
        for path in files(
            &root
                .join("test")
                .join(directory.as_str().context("directory")?),
        )? {
            let id = path
                .strip_prefix(&root)?
                .to_string_lossy()
                .replace('\\', "/");
            if filter.as_ref().is_some_and(|f| !f.is_match(&id)) {
                continue;
            }
            let source = std::fs::read_to_string(&path)?;
            let meta = metadata(&source)?;
            let skip = if meta.flags.iter().any(|f| f == "module") {
                Some("module")
            } else if source.contains("$262") {
                Some("requires $262")
            } else if meta.features.iter().any(|f| !allowed.contains(f.as_str())) {
                Some("feature outside selected ES2015/supported extensions")
            } else {
                None
            };
            let (status, detail) = if let Some(reason) = skip {
                ("skipped".into(), reason.into())
            } else {
                let mut includes = String::new();
                for name in &meta.includes {
                    ensure!(
                        !name.contains(['/', '\\']) && !name.contains(".."),
                        "invalid harness include"
                    );
                    if name == "compareArray.js" {
                        continue;
                    }
                    includes.push_str(&std::fs::read_to_string(root.join("harness").join(name))?);
                    includes.push('\n');
                }
                execute(&source, &meta, &includes)
            };
            *counts.entry(status.clone()).or_default() += 1;
            let features = if meta.features.is_empty() {
                vec!["core".into()]
            } else {
                meta.features.clone()
            };
            for feature in features {
                *groups
                    .entry(feature)
                    .or_default()
                    .entry(status.clone())
                    .or_default() += 1;
            }
            cases.push(json!({"id":id,"source_hash":hash(&source),"features":meta.features,"flags":meta.flags,"includes":meta.includes,"status":status,"detail":detail}));
        }
    }
    cases.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let report = json!({"revision":revision,"selection_hash":hash(&format!("{}|{}",config,std::env::var("VM_TEST262_FILTER").unwrap_or_default())),"harness":"minimal host hooks v1; script compiled using VM strict module mode; not full test262 conformance","counts":counts,"features":groups,"cases":cases});
    let path =
        PathBuf::from(std::env::var("VM_TEST262_REPORT").context("VM_TEST262_REPORT is required")?);
    write_report(&report, &path)?;
    println!("test262 subset: {}", report["counts"]);
    if let Ok(base) = std::env::var("VM_TEST262_BASELINE") {
        compare(&report, Path::new(&base))?;
    }
    Ok(())
}

#[test]
#[ignore = "set VM_CORPUS_JSONL and VM_CORPUS_REPORT; compiles real source without executing it"]
fn model_source_corpus() -> Result<()> {
    let input = std::env::var("VM_CORPUS_JSONL").context("VM_CORPUS_JSONL is required")?;
    let text = std::fs::read_to_string(input)?;
    let mut cases = vec![];
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for line in text.lines() {
        let row: Value = serde_json::from_str(line)?;
        let source = row["source"].as_str().context("source")?;
        let result = super::compile_with_libraries(source, super::libraries::builtin());
        let status = if result.is_ok() {
            "pass"
        } else {
            "compile_rejected"
        };
        let origin = row["origin"].as_str().unwrap_or("unknown");
        *counts
            .entry(origin.into())
            .or_default()
            .entry(status.into())
            .or_default() += 1;
        cases.push(json!({"id":row["id"],"source_hash":hash(source),"origin":origin,"status":status,"detail":result.err().map(|e|e.to_string())}));
    }
    let report = json!({"revision":hash(&text),"selection_hash":hash("unmodified source; compile_with_libraries; builtin catalog"),"counts":counts,"cases":cases});
    write_report(
        &report,
        Path::new(&std::env::var("VM_CORPUS_REPORT").context("VM_CORPUS_REPORT is required")?),
    )?;
    println!("model corpus: {}", report["counts"]);
    if let Ok(base) = std::env::var("VM_CORPUS_BASELINE") {
        compare(&report, Path::new(&base))?;
    }
    Ok(())
}

#[test]
fn vm_stage5fix_native_done_reaction_and_rejection(){
 let m=Metadata{flags:vec!["async".into()],..Default::default()};
 for source in ["Promise.resolve().then($DONE,$DONE);","class C{async m(x=(function(){throw new Test262Error('parameter');})()){throw new Error('body ran');}}new C().m().then(()=>{throw new Error('resolved');},e=>assert.sameValue(e.constructor,Test262Error)).then($DONE,$DONE);"] {
  let result=execute(source,&m,"");assert_eq!(result.0,"pass","{}",result.1);
 }
}

#[test]
fn vm_stage5fix_harness_error_constructor_identity(){let result=execute("const e=new Test262Error('x');assert.sameValue(e.constructor,Test262Error);assert.sameValue(Object.getPrototypeOf(e),Test262Error.prototype);assert(e instanceof Test262Error);",&Metadata::default(),"");assert_eq!(result.0,"pass","{}",result.1);}
