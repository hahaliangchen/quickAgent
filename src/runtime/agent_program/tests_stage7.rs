use super::*;
macro_rules! async_iteration_test {
    ($name:ident,$source:literal,$expected:expr) => {
        #[test]
        fn $name() {
            assert_eq!(returned(runtime($source)), $expected);
        }
    };
}
async_iteration_test!(
    vm_stage7_lazy_queue,
    r#"async function main(){let n=0;async function* g(){n++;yield Promise.resolve(1);yield 2;return 3;}const i=g(),before=n,a=i.next(),b=i.next(),c=i.next();return [before,a instanceof Promise,await a,await b,await c,await i.next(),n];}"#,
    json!([0,true,{"value":1,"done":false},{"value":2,"done":false},{"value":3,"done":true},{"done":true},1])
);
async_iteration_test!(
    vm_stage7_next_input,
    r#"async function main(){const p=Promise.resolve(7);async function* g(){const x=yield 1;yield x===p;}const i=g();await i.next();return (await i.next(p)).value;}"#,
    json!(true)
);
async_iteration_test!(
    vm_stage7_throw_finally,
    r#"async function main(){const e={};async function* g(){try{yield 1;}catch(x){yield x===e;}finally{yield 3;}}const i=g();return [await i.next(),await i.throw(e),await i.next(),await i.next()];}"#,
    json!([{"value":1,"done":false},{"value":true,"done":false},{"value":3,"done":false},{"done":true}])
);
async_iteration_test!(
    vm_stage7_return_reject,
    r#"async function main(){const log=[];async function* g(){try{yield 1;}catch(e){log.push(e);yield 2;}finally{log.push('finally');}}const i=g();await i.next();const a=await i.return(Promise.reject('bad'));const before=log.slice();await i.next();return [a,before,log];}"#,
    json!([{"value":2,"done":false},["bad"],["bad","finally"]])
);
async_iteration_test!(
    vm_stage7_return_expression,
    r#"async function main(){async function* g(){try{return Promise.reject('x');}catch(e){yield e;return 8;}}const i=g();return [await i.next(),await i.next()];}"#,
    json!([{"value":"x","done":false},{"value":8,"done":true}])
);
async_iteration_test!(
    vm_stage7_yield_reject,
    r#"async function main(){async function* g(){try{yield Promise.reject(9);}catch(e){yield e;}}const i=g();return [await i.next(),await i.next()];}"#,
    json!([{"value":9,"done":false},{"done":true}])
);
async_iteration_test!(
    vm_stage7_completed_queue,
    r#"async function main(){let release;const p=new Promise(r=>release=r);async function* g(){throw 'no';}const i=g(),a=i.return(p),b=i.next(),c=i.throw(8);const caught=c.catch(e=>e);release(7);return [await a,await b,await caught];}"#,
    json!([{"value":7,"done":true},{"done":true},8])
);
async_iteration_test!(
    vm_stage7_body_failure,
    r#"async function main(){async function* g(){await 0;throw 7;}const i=g(),a=i.next().catch(e=>e),b=i.next(),c=i.return(8);return [await a,await b,await c];}"#,
    json!([7,{"done":true},{"value":8,"done":true}])
);
async_iteration_test!(
    vm_stage7_brand,
    r#"async function main(){async function* g(){}const f=g().next;let sync=false,p;try{p=f.call({});}catch(e){sync=true;}let rejected=false;try{await p;}catch(e){rejected=e instanceof TypeError;}return [sync,p instanceof Promise,rejected];}"#,
    json!([false, true, true])
);
async_iteration_test!(
    vm_stage7_prototypes,
    r#"async function main(){async function* g(){yield 1;}const i=g(),p=Object.getPrototypeOf(g.prototype);return [i[Symbol.asyncIterator]()===i,i[Symbol.iterator]===undefined,Object.prototype.toString.call(i),Object.prototype.toString.call(g),p.next.length,(await i.next()).value];}"#,
    json!([
        true,
        true,
        "[object AsyncGenerator]",
        "[object AsyncGeneratorFunction]",
        1,
        1
    ])
);
async_iteration_test!(
    vm_stage7_methods,
    r#"async function main(){class A{m(){return 7;}}class B extends A{async *g(){yield super.m();}}const o={n:8,async *g(){yield this.n;}};return [(await new B().g().next()).value,(await o.g().next()).value];}"#,
    json!([7, 8])
);
async_iteration_test!(
    vm_stage7_sync_fallback,
    r#"async function main(){const out=[];for await(const x of [1,Promise.resolve(2),3])out.push(x);return out;}"#,
    json!([1, 2, 3])
);
async_iteration_test!(
    vm_stage7_async_preferred,
    r#"async function main(){const p=Promise.resolve(7),o={[Symbol.iterator](){throw 9;},[Symbol.asyncIterator](){let n=0;return {next(){return Promise.resolve(n++?{done:true}:{done:false,value:p});}};}};const out=[];for await(const x of o)out.push(x===p);return out;}"#,
    json!([true])
);
async_iteration_test!(
    vm_stage7_break_close,
    r#"async function main(){const log=[];async function* g(){try{yield 1;yield 2;}finally{await 0;log.push('closed');}}for await(const x of g()){log.push(x);break;}return log;}"#,
    json!([1, "closed"])
);
async_iteration_test!(
    vm_stage7_throw_close,
    r#"async function main(){const o={[Symbol.asyncIterator](){return {next(){return Promise.resolve({value:1,done:false});},return(){return Promise.reject('close');}};}};try{for await(const x of o)throw 'body';}catch(e){return e;}}"#,
    json!("body")
);
async_iteration_test!(
    vm_stage7_close_rejection,
    r#"async function main(){const o={[Symbol.asyncIterator](){return {next(){return {value:1,done:false};},return(){return Promise.reject('close');}};}};try{for await(const x of o)break;}catch(e){return e;}}"#,
    json!("close")
);
async_iteration_test!(
    vm_stage7_next_error_no_close,
    r#"async function main(){let n=0;const o={[Symbol.asyncIterator](){return {next(){return Promise.reject('next');},return(){n++;return {};}};}};try{for await(const x of o){}}catch(e){return [e,n];}}"#,
    json!(["next", 0])
);
async_iteration_test!(
    vm_stage7_return_continue,
    r#"async function main(){const log=[];async function* g(){try{yield 1;yield 2;yield 3;}finally{log.push('close');}}async function f(){for await(const x of g()){if(x===1)continue;return x;}}return [await f(),log];}"#,
    json!([2, ["close"]])
);
async_iteration_test!(
    vm_stage7_delegate_sync,
    r#"async function main(){async function* g(){yield* [Promise.resolve(1),2];return 3;}const i=g();return [await i.next(),await i.next(),await i.next()];}"#,
    json!([{"value":1,"done":false},{"value":2,"done":false},{"value":3,"done":true}])
);
async_iteration_test!(
    vm_stage7_delegate_return,
    r#"async function main(){const log=[];async function* inner(){try{yield 1;}finally{log.push('closed');}}async function* outer(){yield* inner();}const i=outer();return [await i.next(),await i.return(Promise.resolve(8)),log];}"#,
    json!([{"value":1,"done":false},{"value":8,"done":true},["closed"]])
);
async_iteration_test!(
    vm_stage7_delegate_throw,
    r#"async function main(){async function* inner(){try{yield 1;}catch(e){yield e;return 8;}}async function* outer(){return yield* inner();}const i=outer();return [await i.next(),await i.throw(7),await i.next()];}"#,
    json!([{"value":1,"done":false},{"value":7,"done":false},{"value":8,"done":true}])
);
async_iteration_test!(
    vm_stage7_reentrant,
    r#"async function main(){let i,p;async function* g(){p=i.next();yield 1;yield 2;}i=g();const a=await i.next();return [a,await p,await i.next()];}"#,
    json!([{"value":1,"done":false},{"value":2,"done":false},{"done":true}])
);

fn snapshot(source: &str, responses: &[Value]) -> Value {
    let mut run = runtime(source);
    for response in responses {
        assert!(matches!(run.next(), State::Action(_)));
        run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(response.clone(), false).unwrap();
    }
    returned(run)
}
#[test]
fn vm_stage7_snapshot_queue() {
    let mut run = runtime(
        r#"async function main(){let calls=0;async function* g(){calls++;await 0;const x=io.read({});yield x.n;yield 8;return 9;}const i=g(),a=i.next(),b=i.next(),c=i.next();return [await a,await b,await c,calls];}"#,
    );
    assert!(matches!(run.next(), State::Action(_)));
    let saved = serde_json::to_value(run).unwrap();
    assert!(
        saved["heap"]["generators"]
            .as_object()
            .unwrap()
            .values()
            .any(|g| g["queue"].as_array().unwrap().len() == 3),
        "snapshot must include queued next requests"
    );
    run = serde_json::from_value(saved).unwrap();
    run.resume(json!({"n":7}), false).unwrap();
    assert_eq!(
        returned(run),
        json!([{"value":7,"done":false},{"value":8,"done":false},{"value":9,"done":true},1])
    );
}
#[test]
fn vm_stage7_snapshot_gc() {
    assert_eq!(
        snapshot(
            r#"async function main(){async function* g(){const o={n:7};yield 1;yield o.n;}const i=g();await i.next();for(let n=0;n<800;n++){const garbage={n};}io.read({});return [await i.next(),await i.next()];}"#,
            &[json!({})]
        ),
        json!([{"value":7,"done":false},{"done":true}])
    );
}
#[test]
fn vm_stage7_snapshot_close() {
    assert_eq!(
        snapshot(
            r#"async function main(){const log=[];async function* g(){try{yield 1;}finally{log.push(io.read({}).n);}}for await(const x of g()){log.push(x);break;}return log;}"#,
            &[json!({"n":7})]
        ),
        json!([1, 7])
    );
}
#[test]
fn vm_stage7_snapshot_delegate() {
    assert_eq!(
        snapshot(
            r#"async function main(){async function* inner(){yield io.read({}).n;yield io.read({}).n;}async function* outer(){yield* inner();}const i=outer();return [await i.next(),await i.next(),await i.next()];}"#,
            &[json!({"n":7}), json!({"n":8})]
        ),
        json!([{"value":7,"done":false},{"value":8,"done":false},{"done":true}])
    );
}
#[test]
fn vm_stage7_snapshot_finally() {
    assert_eq!(
        snapshot(
            r#"async function main(){async function* g(){try{yield 1;}finally{yield io.read({}).n;}}const i=g();await i.next();const a=i.return(Promise.resolve(9)),b=i.next();return [await a,await b];}"#,
            &[json!({"n":7})]
        ),
        json!([{"value":7,"done":false},{"value":9,"done":true}])
    );
}
async_iteration_test!(
    vm_stage7_delegated_promise_identity,
    r#"async function main(){const p=Promise.resolve(7);const o={[Symbol.asyncIterator](){return {next(){return {value:p,done:false};}};}};async function* g(){yield* o;}return (await g().next()).value===p;}"#,
    json!(true)
);
async_iteration_test!(
    vm_stage7_thenable_yield,
    r#"async function main(){async function* g(){yield {then(resolve,reject){resolve(7);reject(8);throw 9;}};}return (await g().next()).value;}"#,
    json!(7)
);
async_iteration_test!(
    vm_stage7_then_getter_throw,
    r#"async function main(){const e={};async function* g(){try{yield {get then(){throw e;}};}catch(x){yield x===e;}}return (await g().next()).value;}"#,
    json!(true)
);
async_iteration_test!(
    vm_stage7_promise_constructor_getter,
    r#"async function main(){const p=Promise.resolve(7);Object.defineProperty(p,'constructor',{get(){throw new Error('broken');}});async function* g(){yield 1;}try{await g().return(p);}catch(e){return e.message;}}"#,
    json!("broken")
);
async_iteration_test!(
    vm_stage7_bare_return_ticks,
    r#"async function main(){const log=[];const ticks=Promise.resolve().then(()=>log.push('t1')).then(()=>log.push('t2'));async function* a(){}async function* b(){return;}async function* c(){return undefined;}await Promise.all([ticks,a().next().then(()=>log.push('a')),b().next().then(()=>log.push('b')),c().next().then(()=>log.push('c'))]);return log;}"#,
    json!(["t1", "a", "b", "t2", "c"])
);
async_iteration_test!(
    vm_stage7_for_await_ticks,
    r#"async function main(){const log=[];const ticks=Promise.resolve().then(()=>log.push('t1')).then(()=>log.push('t2'));async function f(){const o={[Symbol.asyncIterator](){return [1][Symbol.iterator]();}};log.push('pre');for await(const x of o){log.push('body');}log.push('post');}await Promise.all([ticks,f()]);return log;}"#,
    json!(["pre", "t1", "body", "t2", "post"])
);
#[test]
fn vm_stage7_snapshot_completed_return_wait() {
    assert_eq!(
        snapshot(
            r#"async function main(){let release;const p=new Promise(r=>release=r);async function* g(){throw 9;}const i=g(),a=i.return(p),b=i.next();queueMicrotask(()=>release(io.read({}).n));return [await a,await b];}"#,
            &[json!({"n":7})]
        ),
        json!([{"value":7,"done":true},{"done":true}])
    );
}
#[test]
fn vm_stage7_snapshot_thenable() {
    assert_eq!(
        snapshot(
            r#"async function main(){async function* g(){yield {then(resolve){resolve(io.read({}).n);}};}return await g().next();}"#,
            &[json!({"n":7})]
        ),
        json!({"value":7,"done":false})
    );
}
async_iteration_test!(
    vm_stage7_throw_promise_identity,
    r#"async function main(){const e=Promise.resolve(7);async function* g(){try{yield 1;}catch(x){yield x===e;}}const i=g();await i.next();return (await i.throw(e)).value;}"#,
    json!(true)
);
async_iteration_test!(
    vm_stage7_parameter_error_sync,
    r#"async function main(){let caught=false;function fail(){throw 7;}async function* g(x=fail()){yield x;}try{g();}catch(e){caught=e===7;}return caught;}"#,
    json!(true)
);
async_iteration_test!(
    vm_stage7_invalid_next_result,
    r#"async function main(){let closed=0;const o={[Symbol.asyncIterator](){return {next(){return 7;},return(){closed++;return {};}};}};try{for await(const x of o){}}catch(e){return [e instanceof TypeError,closed];}}"#,
    json!([true, 0])
);
async_iteration_test!(
    vm_stage7_done_getter_failure,
    r#"async function main(){let closed=0;const o={[Symbol.asyncIterator](){return {next(){return {get done(){throw 7;}};},return(){closed++;return {};}};}};try{for await(const x of o){}}catch(e){return [e,closed];}}"#,
    json!([7, 0])
);
async_iteration_test!(
    vm_stage7_value_getter_failure,
    r#"async function main(){let closed=0;const o={[Symbol.asyncIterator](){return {next(){return {done:false,get value(){throw 7;}};},return(){closed++;return {};}};}};try{for await(const x of o){}}catch(e){return [e,closed];}}"#,
    json!([7, 0])
);
async_iteration_test!(
    vm_stage7_delegate_missing_throw_closes,
    r#"async function main(){const log=[];const o={[Symbol.asyncIterator](){return {next(){return {value:1,done:false};},async return(){await 0;log.push('closed');return {};}};}};async function* g(){yield* o;}const i=g();await i.next();try{await i.throw(7);}catch(e){return [e instanceof TypeError,log];}}"#,
    json!([true, ["closed"]])
);
async_iteration_test!(
    vm_stage7_sync_brand_rejects_async,
    r#"async function main(){async function* a(){}function* b(){}let one=false,two=false;try{b().next.call(a());}catch(e){one=e instanceof TypeError;}try{await a().next.call(b());}catch(e){two=e instanceof TypeError;}return [one,two];}"#,
    json!([true, true])
);
#[test]
fn vm_stage7_snapshot_async_next_promise() {
    assert_eq!(
        snapshot(
            r#"async function main(){let release;const p=new Promise(r=>release=r);let calls=0;const o={[Symbol.asyncIterator](){return {next(){return calls++?{done:true}:p;}};}};queueMicrotask(()=>release({done:false,value:io.read({}).n}));const out=[];for await(const x of o)out.push(x);return [out,calls];}"#,
            &[json!({"n":7})]
        ),
        json!([[7], 2])
    );
}
#[test]
fn vm_stage7_snapshot_constructor_getter() {
    assert_eq!(
        snapshot(
            r#"async function main(){const p=Promise.resolve(7);let gets=0;Object.defineProperty(p,'constructor',{get(){gets++;io.read({});return Promise;}});async function* g(){yield p;}return [await g().next(),gets];}"#,
            &[json!({"n":7})]
        ),
        json!([{"value":7,"done":false},1])
    );
}
#[test]
fn vm_stage7_snapshot_cached_next() {
    assert_eq!(
        snapshot(
            r#"async function main(){let gets=0,n=0;const o={[Symbol.asyncIterator](){return {get next(){gets++;return ()=>({done:n>0,value:n++});}};}};const out=[];for await(const x of o){out.push(x);io.read({});}return [out,gets];}"#,
            &[json!({"n":7})]
        ),
        json!([[0], 1])
    );
}
#[test]
fn vm_stage7_snapshot_host_failure() {
    let mut run = runtime(
        r#"async function main(){async function* g(){try{io.read({});}catch(e){yield e.message;}finally{yield 8;}}const i=g(),a=i.next(),b=i.next();return [await a,await b,await i.next()];}"#,
    );
    assert!(matches!(run.next(), State::Action(_)));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"message":"read failed"}), true).unwrap();
    assert_eq!(
        returned(run),
        json!([{"value":"read failed","done":false},{"value":8,"done":false},{"done":true}])
    );
}
async_iteration_test!(
    vm_stage7_resolving_function_shape,
    r#"async function main(){let resolve,reject;async function* g(){yield {then(a,b){resolve=a;reject=b;a(7);}};}await g().next();return [resolve.name,resolve.length,reject.name,reject.length];}"#,
    json!(["", 1, "", 1])
);
async_iteration_test!(
    vm_stage7_sync_fallback_ticks,
    r#"async function main(){const log=[];const ticks=Promise.resolve().then(()=>log.push('t1')).then(()=>log.push('t2')).then(()=>log.push('t3')).then(()=>log.push('t4'));async function f(){log.push('pre');for await(const x of [Promise.resolve(1)])log.push('body');log.push('post');}await Promise.all([ticks,f()]);return log;}"#,
    json!(["pre", "t1", "t2", "body", "t3", "t4", "post"])
);
async_iteration_test!(
    vm_stage7_queued_values_gc,
    r#"async function main(){async function* g(){yield 1;const x=yield 2;for(let n=0;n<1800;n++){const garbage=[n];}yield x.n;}const i=g(),a=i.next(),b=i.next(),c=i.next({n:7});return [await a,await b,await c];}"#,
    json!([{"value":1,"done":false},{"value":2,"done":false},{"value":7,"done":false}])
);

async_iteration_test!(
    vm_stage7_generator_callback_is_not_async_callback,
    r#"async function main(){const out=[];[1,2].forEach(async function* (x){out.push(x);yield x;});const a=[7].map(async function* (x){yield x;});return [out,(await a[0].next()).value];}"#,
    json!([[], 7])
);

#[test]
fn vm_stage7_unhandled_generator_error_keeps_source_location() {
    let mut run = runtime(
        "async function main(){
async function* g(){
await 0;
throw 7;
}
g().next();return 0;}",
    );
    let State::Failed(error) = run.next() else {
        panic!("unhandled generator rejection must fail");
    };
    assert_eq!(error["location"]["line"], json!(4), "{error}");
    assert!(
        error["stack"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["function"].as_str().is_some_and(|n| n.contains("g@"))),
        "{error}"
    );
}
