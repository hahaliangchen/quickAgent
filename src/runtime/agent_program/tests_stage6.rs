use super::*;

macro_rules! generator_test {
    ($name:ident, $source:literal, $expected:expr) => {
        #[test]
        fn $name() {
            assert_eq!(returned(runtime($source)), $expected);
        }
    };
}

generator_test!(
    vm_stage6_lazy_body_and_next_values,
    r#"function main(){let n=0;function* g(){n++;const x=yield 1;yield x+2;return 9;}const it=g(),before=n;return [before,it.next(999),n,it.next(5),it.next(),it.next(7)];}"#,
    json!([0,{"value":1,"done":false},1,{"value":7,"done":false},{"value":9,"done":true},{"done":true}])
);
generator_test!(
    vm_stage6_bare_yield,
    r#"function main(){function* g(){yield;return yield 2;}const i=g();return [i.next(),i.next(),i.next(7)];}"#,
    json!([{"done":false},{"value":2,"done":false},{"value":7,"done":true}])
);
generator_test!(
    vm_stage6_parameters_initialize_at_call,
    r#"function main(){let n=0;function* g(x=++n){yield x;}const a=g(),b=g();let bad=false;function fail(){throw new ReferenceError('parameter');}function* h(x=fail()){yield x;}try{h();}catch(e){bad=e instanceof ReferenceError;}return [n,a.next().value,b.next().value,bad];}"#,
    json!([2, 1, 2, true])
);
generator_test!(
    vm_stage6_expression_methods_this_and_closure,
    r#"function main(){const o={n:7,*g(x){yield this.n+x;}},f=function* named(){yield named===f;};class C{*g(){yield 3;}static *g(){yield 4;}}return [o.g(2).next().value,f().next().value,new C().g().next().value,C.g().next().value];}"#,
    json!([9, true, 3, 4])
);
generator_test!(
    vm_stage6_throw_is_catchable_and_preserves_identity,
    r#"function main(){const e={n:7};function* g(){try{yield 1;}catch(x){yield x===e;}return 3;}const i=g();return [i.next(),i.throw(e),i.next()];}"#,
    json!([{"value":1,"done":false},{"value":true,"done":false},{"value":3,"done":true}])
);
generator_test!(
    vm_stage6_throw_before_start_and_after_completion,
    r#"function main(){let n=0;function* g(){try{n++;yield 1;}finally{n++;}}const i=g(),e={};let same=0;try{i.throw(e);}catch(x){if(x===e)same++;}try{i.throw(e);}catch(x){if(x===e)same++;}return [n,same,i.next(),i.return(8)];}"#,
    json!([0,2,{"done":true},{"value":8,"done":true}])
);
generator_test!(
    vm_stage6_return_before_start,
    r#"function main(){let n=0;function* g(){try{n++;yield 1;}finally{n++;}}const i=g();return [i.return(7),i.next(),n];}"#,
    json!([{"value":7,"done":true},{"done":true},0])
);
generator_test!(
    vm_stage6_return_runs_finally_without_catch,
    r#"function main(){const log=[];function* g(){try{try{yield 1;}catch(e){log.push('catch');}}finally{log.push('finally');}}const i=g();i.next();return [i.return(7),log,i.next()];}"#,
    json!([{"value":7,"done":true},["finally"],{"done":true}])
);
generator_test!(
    vm_stage6_return_pending_across_finally_yields,
    r#"function main(){function* g(){try{yield 1;}finally{yield 2;yield 3;}}const i=g();return [i.next(),i.return(9),i.next(55),i.next(66),i.next()];}"#,
    json!([{"value":1,"done":false},{"value":2,"done":false},{"value":3,"done":false},{"value":9,"done":true},{"done":true}])
);
generator_test!(
    vm_stage6_finally_can_override_return_and_throw,
    r#"function main(){function* g(){try{yield 1;}finally{yield 2;return 8;}}const i=g();i.next();const a=i.throw(7),b=i.next();function* h(){try{yield 1;}finally{throw 9;}}const j=h();j.next();let caught;try{j.return(2);}catch(e){caught=e;}return [a,b,caught,j.next()];}"#,
    json!([{"value":2,"done":false},{"value":8,"done":true},9,{"done":true}])
);
generator_test!(
    vm_stage6_reentrant_resume_is_type_error,
    r#"function main(){let i;const out=[];function* g(){for(const name of ['next','return','throw']){try{i[name](1);}catch(e){out.push(e instanceof TypeError);}}yield 7;}i=g();return [i.next().value,out];}"#,
    json!([7, [true, true, true]])
);
generator_test!(
    vm_stage6_protocol_consumers_and_close,
    r#"function main(){const log=[];function* g(){try{yield 1;yield 2;yield 3;}finally{log.push('close');}}const a=[...g()],b=Array.from(g(),x=>x*2);for(const x of g())break;const [v]=g();return [a,b,v,log];}"#,
    json!([
        [1, 2, 3],
        [2, 4, 6],
        1,
        ["close", "close", "close", "close"]
    ])
);
generator_test!(
    vm_stage6_interleaved_instances,
    r#"function main(){function* g(n){let v=n;while(true)v+=yield v;}const a=g(1),b=g(10);return [a.next().value,b.next().value,a.next(2).value,b.next(4).value,a.next(3).value];}"#,
    json!([1, 10, 3, 14, 6])
);
generator_test!(
    vm_stage6_generator_function_prototype_and_brand,
    r#"function main(){function* g(a,b){}const i=g(),p=Object.getPrototypeOf(g),gp=Object.getPrototypeOf(g.prototype);let bad=0;for(const name of ['next','return','throw'])try{i[name].call({});}catch(e){if(e instanceof TypeError)bad++;}try{new g();}catch(e){if(e instanceof TypeError)bad++;}return [g.name,g.length,Object.getPrototypeOf(i)===g.prototype,Object.getPrototypeOf(p)===Function.prototype,p.prototype===gp,i[Symbol.iterator]()===i,Object.prototype.toString.call(i),Object.prototype.toString.call(g),Object.keys(i),bad];}"#,
    json!([
        "g",
        2,
        true,
        true,
        true,
        true,
        "[object Generator]",
        "[object GeneratorFunction]",
        [],
        4
    ])
);
generator_test!(
    vm_stage6_changed_generator_prototype,
    r#"function main(){function* g(){yield 7;}const old=g.prototype,a=g();g.prototype={custom:3};const b=g();g.prototype=null;const c=g();return [Object.getPrototypeOf(a)===old,Object.getPrototypeOf(b)===g.prototype,b.custom,typeof b.next,c.next().value];}"#,
    json!([true, false, 3, "undefined", 7])
);
generator_test!(
    vm_stage6_yield_star_nested_and_completion,
    r#"function main(){function* inner(){yield 2;return 8;}function* g(){yield 1;const x=yield* inner();yield x;return yield* [3,4];}const i=g();return [i.next(),i.next(),i.next(),i.next(),i.next(),i.next()];}"#,
    json!([{"value":1,"done":false},{"value":2,"done":false},{"value":8,"done":false},{"value":3,"done":false},{"value":4,"done":false},{"done":true}])
);
generator_test!(
    vm_stage6_yield_star_throw_and_return_forwarding,
    r#"function main(){const calls=[];const source={[Symbol.iterator](){return this;},next(x){calls.push(['next',x]);return {value:1,done:false};},throw(x){calls.push(['throw',x]);return {value:2,done:false};},return(x){calls.push(['return',x]);return {value:x+1,done:true};}};function* g(){try{return yield* source;}finally{calls.push(['finally']);}}const i=g();return [i.next(),i.next(4),i.throw(5),i.return(7),calls];}"#,
    json!([{"value":1,"done":false},{"value":1,"done":false},{"value":2,"done":false},{"value":8,"done":true},[["next",null],["next",4],["throw",5],["return",7],["finally"]]])
);
generator_test!(
    vm_stage6_yield_star_missing_throw_closes,
    r#"function main(){let closed=0;const source={[Symbol.iterator](){return this;},next(){return {value:1,done:false};},return(){closed++;return {};}};function* g(){yield* source;}const i=g();i.next();let bad=false;try{i.throw(7);}catch(e){bad=e instanceof TypeError;}return [closed,bad,i.next()];}"#,
    json!([1,true,{"done":true}])
);
generator_test!(
    vm_stage6_yield_star_preserves_result_identity_and_done_getter,
    r#"function main(){let reads=0;const step={value:7,get done(){reads++;return false;}},source={[Symbol.iterator](){return this;},next(){return step;}};function* g(){yield* source;}const i=g(),result=i.next();return [result===step,reads,result.value];}"#,
    json!([true, 1, 7])
);
generator_test!(
    vm_stage6_yield_star_return_done_false_resumes_normally,
    r#"function main(){const calls=[];const source={[Symbol.iterator](){return this;},next(x){calls.push(['next',x]);return {value:2,done:calls.length>2};},return(x){calls.push(['return',x]);return {value:3,done:false};}};function* g(){const v=yield* source;return v+10;}const i=g();return [i.next(),i.return(9),i.next(4),calls];}"#,
    json!([{"value":2,"done":false},{"value":3,"done":false},{"value":12,"done":true},[["next",null],["return",9],["next",4]]])
);
generator_test!(
    vm_stage6_yield_star_non_object_result_throws,
    r#"function main(){function* g(){try{yield* {[Symbol.iterator](){return {next(){return 1;}};}};}catch(e){yield e instanceof TypeError;}}return g().next().value;}"#,
    json!(true)
);
generator_test!(
    vm_stage6_generator_method_super,
    r#"function main(){class A{m(){return this.n;}}class B extends A{constructor(){super();this.n=7;}*g(){yield super.m();}}return new B().g().next().value;}"#,
    json!(7)
);
generator_test!(
    vm_stage6_generator_descriptors,
    r#"function main(){function* g(){}const fp=Object.getPrototypeOf(g),gp=Object.getPrototypeOf(g.prototype),out=[];for(const [obj,key] of [[g,'prototype'],[g,'name'],[g,'length'],[fp,'prototype'],[fp,'constructor'],[gp,'constructor'],[gp,'next'],[gp,'return'],[gp,'throw']]){const d=Object.getOwnPropertyDescriptor(obj,key);out.push([d.writable,d.enumerable,d.configurable]);}return out;}"#,
    json!([
        [true, false, false],
        [false, false, true],
        [false, false, true],
        [false, false, true],
        [false, false, true],
        [false, false, true],
        [true, false, true],
        [true, false, true],
        [true, false, true]
    ])
);
generator_test!(
    vm_stage6_return_replaces_pending_return,
    r#"function main(){const log=[];function* g(){try{try{yield 1;}finally{yield 2;log.push('inner');}}finally{log.push('outer');yield 3;}}const i=g();return [i.next(),i.return(7),i.return(8),i.next(99),log];}"#,
    json!([{"value":1,"done":false},{"value":2,"done":false},{"value":3,"done":false},{"value":8,"done":true},["outer"]])
);
generator_test!(
    vm_stage6_yield_star_missing_return_finishes_outer,
    r#"function main(){const log=[];function* g(){try{yield* {[Symbol.iterator](){return this;},next(){return {value:1,done:false};}};log.push('after');}finally{log.push('finally');yield 2;}}const i=g();return [i.next(),i.return(7),i.next(),log];}"#,
    json!([{"value":1,"done":false},{"value":2,"done":false},{"value":7,"done":true},["finally"]])
);
generator_test!(
    vm_stage6_yield_star_cached_next_and_lazy_value,
    r#"function main(){let gets=0,values=0;const step={done:false,get value(){values++;return 3;}},source={[Symbol.iterator](){return this;},get next(){gets++;return function(){return step;};}};function* g(){yield* source;}const i=g(),a=i.next(),b=i.next(7);return [a===step,b===step,gets,values];}"#,
    json!([true, true, 1, 0])
);
generator_test!(
    vm_stage6_yield_star_close_error_precedence,
    r#"function main(){const error={};const source={[Symbol.iterator](){return this;},next(){return {done:false};},return(){throw error;}};function* g(){yield* source;}const i=g();i.next();let same=false;try{i.throw(7);}catch(e){same=e===error;}return [same,i.next()];}"#,
    json!([true,{"done":true}])
);
generator_test!(
    vm_stage6_completed_generators_stay_extensible,
    r#"function main(){function* g(){yield 1;return 2;}const i=g();i.x=7;Object.freeze(i);const out=[i.next(),i.next(),i.next(),i.x,Object.isFrozen(i)];let bad=false;try{structuredClone(i);}catch(e){bad=true;}return [out,bad];}"#,
    json!([[{"value":1,"done":false},{"value":2,"done":true},{"done":true},7,true],true])
);
generator_test!(
    vm_stage6_generator_getter_and_promise_callbacks,
    r#"async function main(){const o={get value(){return (function*(){yield 7;})();}};function* g(){yield 8;}const i=await Promise.resolve().then(g);return [o.value.next().value,i.next().value];}"#,
    json!([7, 8])
);
generator_test!(
    vm_stage6_instance_prototype_read_after_parameter_initialization,
    r#"function main(){function* g(x=(g.prototype={n:7})){yield x;}const old=g.prototype,i=g();return [Object.getPrototypeOf(i)!==old,Object.getPrototypeOf(i)===g.prototype,i.n,i[Symbol.iterator]===undefined];}"#,
    json!([true, true, 7, true])
);
generator_test!(
    vm_stage6_yield_accessors_and_coercion,
    r#"function main(){let gets=0,converts=0;const o={get n(){gets++;return 7;}},v={valueOf(){converts++;return 3;}};function* g(){yield o.n;yield v+2;}const i=g();return [i.next(),i.next(),gets,converts];}"#,
    json!([{"value":7,"done":false},{"value":5,"done":false},1,1])
);

fn saved(mut run: Runtime, response: Value) -> Runtime {
    assert!(matches!(run.next(), State::Action(_)));
    let mut restored: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    restored.resume(response, false).unwrap();
    restored
}

#[test]
fn vm_stage6_snapshot_suspended_start_and_yield_with_gc() {
    let source = r#"function main(){function make(){const o={n:7};return function*(){let x=o.n;yield x;x+=3;yield x;return o.n;};}const g=make(),a=g();for(let i=0;i<400;i++){const garbage=[i];}io.read({});const first=a.next();for(let i=0;i<400;i++){const garbage=[i];}io.read({});return [first,a.next(),a.next()];}"#;
    let run = saved(runtime(source), json!({}));
    let run = saved(run, json!({}));
    assert_eq!(
        returned(run),
        json!([{"value":7,"done":false},{"value":10,"done":false},{"value":7,"done":true}])
    );
}
#[test]
fn vm_stage6_snapshot_executing_host_call_no_replay() {
    let source = r#"function main(){let n=0;function* g(){n++;const r=io.read({path:'once'});yield r.n;return ++n;}const i=g(),v=i.next();return [v,i.next(),n];}"#;
    assert_eq!(
        returned(saved(runtime(source), json!({"n":7}))),
        json!([{"value":7,"done":false},{"value":2,"done":true},2])
    );
}
#[test]
fn vm_stage6_snapshot_finally_pending_return() {
    let source = r#"function main(){function* g(){try{yield 1;}finally{yield 2;const r=io.read({});yield r.n;}}const i=g();i.next();const a=i.return(9);io.read({});const b=i.next();return [a,b,i.next()];}"#;
    let run = saved(runtime(source), json!({}));
    let run = saved(run, json!({"n":7}));
    assert_eq!(
        returned(run),
        json!([{"value":2,"done":false},{"value":7,"done":false},{"value":9,"done":true}])
    );
}
#[test]
fn vm_stage6_snapshot_yield_star_nested_frames_and_throw() {
    let source = r#"function main(){function* inner(){try{yield 1;}catch(e){const r=io.read({});yield e+r.n;}return 9;}function* outer(){return yield* inner();}const i=outer(),a=i.next();io.read({});const b=i.throw(3);return [a,b,i.next()];}"#;
    let run = saved(runtime(source), json!({}));
    let run = saved(run, json!({"n":4}));
    assert_eq!(
        returned(run),
        json!([{"value":1,"done":false},{"value":7,"done":false},{"value":9,"done":true}])
    );
}
#[test]
fn vm_stage6_snapshot_pending_throw_through_finally() {
    let source = r#"function main(){const error={n:7};function* g(){try{yield 1;}finally{yield 2;}}const i=g();i.next();const a=i.throw(error);io.read({});let same=false;try{i.next();}catch(e){same=e===error;}return [a,same,i.next()];}"#;
    assert_eq!(
        returned(saved(runtime(source), json!({}))),
        json!([{"value":2,"done":false},true,{"done":true}])
    );
}
#[test]
fn vm_stage6_async_generator_still_rejected() {
    // The stage-6 rejection guard now applies only to AI declarations.
    assert!(compile("/** @module */ async function* g(){yield 1;} function main(){return 1;}").is_err());
}

#[test]
fn vm_stage6_snapshot_generator_only_object_roots() {
    let source = r#"function main(){function make(){return (function*(){const key=Symbol('private'),local={[key]:{n:7}};yield 1;return local[key].n;})();}const i=make();i.next();for(let n=0;n<400;n++){const garbage=[n];}io.read({});return i.next();}"#;
    assert_eq!(
        returned(saved(runtime(source), json!({}))),
        json!({"value":7,"done":true})
    );
}
#[test]
fn vm_stage6_snapshot_host_error_is_caught_in_generator() {
    let source = r#"function main(){function* g(){try{io.read({});}catch(e){yield e.message;}finally{yield 9;}}const i=g(),a=i.next();return [a,i.next(),i.next()];}"#;
    let mut run = runtime(source);
    assert!(matches!(run.next(), State::Action(_)));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"message":"read failed"}), true).unwrap();
    assert_eq!(
        returned(run),
        json!([{"value":"read failed","done":false},{"value":9,"done":false},{"done":true}])
    );
}
#[test]
fn vm_stage6_snapshot_parameter_initialization_before_instance_creation() {
    let source = r#"function main(){function initialize(){const r=io.read({});g.prototype={n:r.n};return r.n;}function* g(x=initialize()){yield x;}const old=g.prototype,i=g();return [Object.getPrototypeOf(i)!==old,Object.getPrototypeOf(i)===g.prototype,i.n];}"#;
    assert_eq!(
        returned(saved(runtime(source), json!({"n":7}))),
        json!([true, true, 7])
    );
}
#[test]
fn vm_stage6_snapshot_yield_getter_host_call() {
    let source = r#"function main(){let gets=0;const o={get n(){gets++;return io.read({}).n;}};function* g(){yield o.n;}const i=g(),result=i.next();return [result,gets,i.next()];}"#;
    assert_eq!(
        returned(saved(runtime(source), json!({"n":7}))),
        json!([{"value":7,"done":false},1,{"done":true}])
    );
}

#[test]
fn vm_stage6_generator_entry_returns_iterator_without_running_body() {
    let mut run = runtime("function* main(){console.log('must not run');throw new Error('body');}");
    assert!(matches!(run.next(), State::Returned(v) if v == json!({})));
    assert!(run.logs().is_empty());
}

#[test]
fn vm_stage6_snapshot_completed_generator_does_not_restart() {
    let source = r#"function main(){let calls=0;function* g(){calls++;return 7;}const i=g(),first=i.next();for(let n=0;n<400;n++){const garbage=[n];}io.read({});return [first,i.next(),i.return(9),calls];}"#;
    assert_eq!(
        returned(saved(runtime(source), json!({}))),
        json!([{"value":7,"done":true},{"done":true},{"value":9,"done":true},1])
    );
}

generator_test!(
    vm_stage6_destructuring_elisions_advance_generator,
    r#"function main(){let first=0,second=0;function* g(){first++;yield;second++;}const skip=([,])=>0;skip(g());return [first,second];}"#,
    json!([1, 0])
);
generator_test!(
    vm_stage6_destructuring_elision_propagates_generator_error,
    r#"function main(){const error={};function* g(){throw error;}let same=false;try{(([,])=>{})(g());}catch(e){same=e===error;}return same;}"#,
    json!(true)
);
generator_test!(
    vm_stage6_elision_shape_preserves_comments_defaults_and_super,
    r#"function main(){let n=0;function* g(){n++;yield; n++;yield 2; n++;yield 3;}const f=([x=',' /* ,,] */, , ,])=>x,a=f(g()),before=n;n=0;const h=([x,])=>x;h(g());const trailing=n;class A{m(){return 7;}}class B extends A{m(){const pick=([x=super.m(),,])=>x;return pick(g());}}return [a,before,trailing,new B().m()];}"#,
    json!([",", 3, 1, 7])
);
