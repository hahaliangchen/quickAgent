use super::*;

#[test]
fn vm_stage5fix_native_callback_coercion() {
    assert_eq!(
        returned(runtime(
            r#"async function main(){const o={[Symbol.toPrimitive](){return "ok";}};return [await Promise.resolve(o).then(String),await Promise.resolve({valueOf(){return 7;}}).then(Number)];}"#
        )),
        json!(["ok", 7])
    );
}

#[test]
fn vm_stage5fix_native_callback_undefined() {
    assert_eq!(
        returned(runtime(
            r#"async function main(){const p=Promise.resolve(undefined).then(console.log);await p;return "done";}"#
        )),
        json!("done")
    );
}

#[test]
fn vm_stage5fix_native_callback_class_guard() {
    assert_eq!(
        returned(runtime(
            r#"async function main(){class C{}try{await Promise.resolve(1).then(C);}catch(e){return e instanceof TypeError;}return false;}"#
        )),
        json!(true)
    );
}

#[test]
fn vm_stage5fix_callback_home_object() {
    assert_eq!(
        returned(runtime(
            r#"async function main(){class A{static m(){return 7;}}class B extends A{static m(){return super.m();}}return await Promise.resolve().then(B.m);}"#
        )),
        json!(7)
    );
}

#[test]
fn vm_stage5fix_callback_native_type_error_rejects() {
    assert_eq!(
        returned(runtime(
            r#"async function main(){try{await Promise.resolve(Symbol()).then(Number);}catch(e){return [e instanceof TypeError,e.message.includes("Symbol")];}return false;}"#
        )),
        json!([true, true])
    );
}

#[test]
fn vm_stage5fix_callback_async_order() {
    assert_eq!(
        returned(runtime(
            r#"async function main(){const log=[];const p=Promise.resolve().then(async()=>{log.push("callback");return 7;}).then(n=>log.push(n));Promise.resolve().then(()=>log.push("other"));await p;return log;}"#
        )),
        json!(["callback", "other", 7])
    );
}

#[test]
fn vm_stage5fix_callback_real_tool_boundary_kept() {
    assert_eq!(
        returned(runtime(
            r#"import {read} from "host:io";async function main(){try{await Promise.resolve(undefined).then(read);}catch(e){return e.message.includes("arguments must be an object");}return false;}"#
        )),
        json!(true)
    );
}

#[test]
fn vm_stage5fix_callback_dynamic_function_diagnostic() {
    assert_eq!(
        returned(runtime(
            r#"async function main(){try{await Promise.resolve(undefined).then(Function);}catch(e){return [e instanceof EvalError,e.message.includes("Function"),!e.message.includes("JSON")];}return false;}"#
        )),
        json!([true, true, true])
    );
}

#[test]
fn vm_stage5fix_binding_function_arrow_names() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const f=function(){},g=()=>{},h=async()=>{};return [f.name,g.name,h.name];}"#
        )),
        json!(["f", "g", "h"])
    );
}

#[test]
fn vm_stage5fix_parameter_default_names() {
    assert_eq!(
        returned(runtime(
            r#"function main(){function f(fn=function(){},arrow=()=>{},C=class{}){return [fn.name,arrow.name,C.name];}return f();}"#
        )),
        json!(["fn", "arrow", "C"])
    );
}

#[test]
fn vm_stage5fix_array_pattern_default_names() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const [f=function(){},g=()=>{},C=class{}]=[];return [f.name,g.name,C.name];}"#
        )),
        json!(["f", "g", "C"])
    );
}

#[test]
fn vm_stage5fix_object_pattern_default_names() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const {f=function(){},g=()=>{},C=class{},q:renamed=()=>{}}={};return [f.name,g.name,C.name,renamed.name];}"#
        )),
        json!(["f", "g", "C", "renamed"])
    );
}

#[test]
fn vm_stage5fix_named_expression_preserved() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const [f=function Explicit(){},C=class Explicit{},D=class{static name(){return 7;}}]=[];return [f.name,C.name,D.name()];}"#
        )),
        json!(["Explicit", "Explicit", 7])
    );
}

#[test]
fn vm_stage5fix_inferred_name_no_lexical_self() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let f=function(){return f;};const old=f;f=7;let g=()=>g;const prev=g;g=8;return [old(),prev()];}"#
        )),
        json!([7, 8])
    );
}

#[test]
fn vm_stage5fix_object_property_names() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const s=Symbol("key"),o={f:function(){},g:()=>{},C:class{},[s]:function(){}};return [o.f.name,o.g.name,o.C.name,o[s].name];}"#
        )),
        json!(["f", "g", "C", "[key]"])
    );
}

#[test]
fn vm_stage5fix_computed_class_static_name_preserved() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const key={toString(){n++;return "C";}},o={[key]:class{static name(){return 9;}}};return [o.C.name(),n];}"#
        )),
        json!([9, 1])
    );
}

#[test]
fn vm_stage5fix_logical_assignment_names() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let f,g,h;f ||=function(){};g ??=()=>{};h=class{};return [f.name,g.name,h.name];}"#
        )),
        json!(["f", "g", "h"])
    );
}

#[test]
fn vm_stage5fix_method_symbol_names() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const s=Symbol("key"),o={[s](){},get x(){return 7;},set y(v){}};return [o[s].name,Object.getOwnPropertyDescriptor(o,"x").get.name,Object.getOwnPropertyDescriptor(o,"y").set.name];}"#
        )),
        json!(["[key]", "get x", "set y"])
    );
}

#[test]
fn vm_stage5fix_native_coercion_snapshot() {
    let mut run = runtime(
        r#"import {read} from 'host:io';async function main(){const o={valueOf(){return read({path:'x'}).n;}};return await Promise.resolve(o).then(Number);}"#,
    );
    let State::Action(_) = run.next() else {
        panic!("expected read during coercion");
    };
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":7}), false).unwrap();
    assert_eq!(returned(run), json!(7));
}
#[test]
fn vm_stage5fix_unknown_native_diagnostic_before_json() {
    let mut heap = heap::Heap::default();
    let error = js_methods::builtin(&mut heap, "missing.native", &[heap::VmValue::Undefined])
        .unwrap_err()
        .to_string();
    assert!(error.contains("missing.native"), "{error}");
    assert!(!error.contains("JSON"), "{error}");
}

#[test]
fn vm_stage5fix_exponential_basic() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class N extends Number{}return [(123).toExponential(),(123).toExponential(2),(9.5).toExponential(0),(-9.5).toExponential(0),new N(12.5).toExponential(1),(-0).toExponential(2)];}"#
        )),
        json!(["1.23e+2", "1.23e+2", "1e+1", "-1e+1", "1.3e+1", "0.00e+0"])
    );
}

#[test]
fn vm_stage5fix_exponential_extremes() {
    assert_eq!(
        returned(runtime(
            r#"function main(){return [Number.MIN_VALUE.toExponential(),Number.MAX_VALUE.toExponential(2),(1e-300).toExponential(4),NaN.toExponential(101),Infinity.toExponential(-1)];}"#
        )),
        json!(["5e-324", "1.80e+308", "1.0000e-300", "NaN", "Infinity"])
    );
}

#[test]
fn vm_stage5fix_exponential_brand_and_range() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;for(const f of [()=>Number.prototype.toExponential.call({}),()=>(1).toExponential(-1),()=>(1).toExponential(101),()=>(1).toExponential(Symbol())])try{f();}catch(e){if(e instanceof TypeError||e instanceof RangeError)n++;}return [n,Number.prototype.toExponential.length,(0).toExponential(100).length];}"#
        )),
        json!([4, 1, 105])
    );
}

#[test]
fn vm_stage5fix_exponential_argument_order() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const d={valueOf(){n++;return 2;}};const value=Number.prototype.toExponential.call(1.25,d);try{Number.prototype.toExponential.call({},d);}catch(e){}return [value,n];}"#
        )),
        json!(["1.25e+0", 1])
    );
}

#[test]
fn vm_stage5fix_callback_native_error_location() {
    assert_eq!(
        returned(runtime(
            "async function main(){\ntry{\nawait Promise.resolve().then(Function);\n}catch(e){return [e.location.line,e.message.includes('Function')];}\n}"
        )),
        json!([3, true])
    );
}
#[test]
fn vm_stage5fix_callback_tool_error_location() {
    assert_eq!(
        returned(runtime(
            "import {read} from 'host:io';\nasync function main(){\ntry{await Promise.resolve(3).then(read);}catch(e){return e.location.line;}\n}"
        )),
        json!(3)
    );
}

#[test]
fn vm_stage5fix_exponential_conversion_snapshot() {
    let mut run = runtime(
        r#"import {read} from 'host:io';async function main(){const digits={valueOf(){return read({path:'digits'}).n;}};return (1.25).toExponential(digits);}"#,
    );
    let State::Action(_) = run.next() else {
        panic!("expected read during digits conversion");
    };
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":2}), false).unwrap();
    assert_eq!(returned(run), json!("1.25e+0"));
}
#[test]
fn vm_stage5fix_name_metadata_survives_snapshot() {
    let mut run = runtime(
        r#"import {read} from 'host:io';function main(){const {fn=()=>{},C=class{}}={};read({path:'x'});return [fn.name,C.name,fn.length,C.length];}"#,
    );
    assert!(matches!(run.next(), State::Action(_)));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert_eq!(returned(run), json!(["fn", "C", 0, 0]));
}
#[test]
fn vm_stage5fix_name_utf16_and_symbol_accessors() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const s=Symbol('\udc00'),o={get [s](){return 7;},C:class{},f:()=>{}};const n=Object.getOwnPropertyDescriptor(o,s).get.name;return [n.length,n.charCodeAt(5),Object.getOwnPropertyDescriptor(o.f,'name').writable,Object.getOwnPropertyDescriptor(o.C,'name').configurable];}"#
        )),
        json!([7, 56320, false, true])
    );
}

#[test]
fn vm_stage5fix_compound_assignment_does_not_infer_name() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let seen='bad';Function.prototype[Symbol.toPrimitive]=function(){seen=this.name;return 1;};let total=0;total+=function(){};return [seen,total];}"#
        )),
        json!(["", 1])
    );
}
