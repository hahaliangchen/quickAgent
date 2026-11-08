// Standard-JS counterparts of tests_stage7.rs; snapshots/diagnostics use VM tests.
const cases=[
  [
    "vm_stage7_lazy_queue",
    "async function main(){let n=0;async function* g(){n++;yield Promise.resolve(1);yield 2;return 3;}const i=g(),before=n,a=i.next(),b=i.next(),c=i.next();return [before,a instanceof Promise,await a,await b,await c,await i.next(),n];}"
  ],
  [
    "vm_stage7_next_input",
    "async function main(){const p=Promise.resolve(7);async function* g(){const x=yield 1;yield x===p;}const i=g();await i.next();return (await i.next(p)).value;}"
  ],
  [
    "vm_stage7_throw_finally",
    "async function main(){const e={};async function* g(){try{yield 1;}catch(x){yield x===e;}finally{yield 3;}}const i=g();return [await i.next(),await i.throw(e),await i.next(),await i.next()];}"
  ],
  [
    "vm_stage7_return_reject",
    "async function main(){const log=[];async function* g(){try{yield 1;}catch(e){log.push(e);yield 2;}finally{log.push('finally');}}const i=g();await i.next();const a=await i.return(Promise.reject('bad'));const before=log.slice();await i.next();return [a,before,log];}"
  ],
  [
    "vm_stage7_return_expression",
    "async function main(){async function* g(){try{return Promise.reject('x');}catch(e){yield e;return 8;}}const i=g();return [await i.next(),await i.next()];}"
  ],
  [
    "vm_stage7_yield_reject",
    "async function main(){async function* g(){try{yield Promise.reject(9);}catch(e){yield e;}}const i=g();return [await i.next(),await i.next()];}"
  ],
  [
    "vm_stage7_completed_queue",
    "async function main(){let release;const p=new Promise(r=>release=r);async function* g(){throw 'no';}const i=g(),a=i.return(p),b=i.next(),c=i.throw(8);const caught=c.catch(e=>e);release(7);return [await a,await b,await caught];}"
  ],
  [
    "vm_stage7_body_failure",
    "async function main(){async function* g(){await 0;throw 7;}const i=g(),a=i.next().catch(e=>e),b=i.next(),c=i.return(8);return [await a,await b,await c];}"
  ],
  [
    "vm_stage7_brand",
    "async function main(){async function* g(){}const f=g().next;let sync=false,p;try{p=f.call({});}catch(e){sync=true;}let rejected=false;try{await p;}catch(e){rejected=e instanceof TypeError;}return [sync,p instanceof Promise,rejected];}"
  ],
  [
    "vm_stage7_prototypes",
    "async function main(){async function* g(){yield 1;}const i=g(),p=Object.getPrototypeOf(g.prototype);return [i[Symbol.asyncIterator]()===i,i[Symbol.iterator]===undefined,Object.prototype.toString.call(i),Object.prototype.toString.call(g),p.next.length,(await i.next()).value];}"
  ],
  [
    "vm_stage7_methods",
    "async function main(){class A{m(){return 7;}}class B extends A{async *g(){yield super.m();}}const o={n:8,async *g(){yield this.n;}};return [(await new B().g().next()).value,(await o.g().next()).value];}"
  ],
  [
    "vm_stage7_sync_fallback",
    "async function main(){const out=[];for await(const x of [1,Promise.resolve(2),3])out.push(x);return out;}"
  ],
  [
    "vm_stage7_async_preferred",
    "async function main(){const p=Promise.resolve(7),o={[Symbol.iterator](){throw 9;},[Symbol.asyncIterator](){let n=0;return {next(){return Promise.resolve(n++?{done:true}:{done:false,value:p});}};}};const out=[];for await(const x of o)out.push(x===p);return out;}"
  ],
  [
    "vm_stage7_break_close",
    "async function main(){const log=[];async function* g(){try{yield 1;yield 2;}finally{await 0;log.push('closed');}}for await(const x of g()){log.push(x);break;}return log;}"
  ],
  [
    "vm_stage7_throw_close",
    "async function main(){const o={[Symbol.asyncIterator](){return {next(){return Promise.resolve({value:1,done:false});},return(){return Promise.reject('close');}};}};try{for await(const x of o)throw 'body';}catch(e){return e;}}"
  ],
  [
    "vm_stage7_close_rejection",
    "async function main(){const o={[Symbol.asyncIterator](){return {next(){return {value:1,done:false};},return(){return Promise.reject('close');}};}};try{for await(const x of o)break;}catch(e){return e;}}"
  ],
  [
    "vm_stage7_next_error_no_close",
    "async function main(){let n=0;const o={[Symbol.asyncIterator](){return {next(){return Promise.reject('next');},return(){n++;return {};}};}};try{for await(const x of o){}}catch(e){return [e,n];}}"
  ],
  [
    "vm_stage7_return_continue",
    "async function main(){const log=[];async function* g(){try{yield 1;yield 2;yield 3;}finally{log.push('close');}}async function f(){for await(const x of g()){if(x===1)continue;return x;}}return [await f(),log];}"
  ],
  [
    "vm_stage7_delegate_sync",
    "async function main(){async function* g(){yield* [Promise.resolve(1),2];return 3;}const i=g();return [await i.next(),await i.next(),await i.next()];}"
  ],
  [
    "vm_stage7_delegate_return",
    "async function main(){const log=[];async function* inner(){try{yield 1;}finally{log.push('closed');}}async function* outer(){yield* inner();}const i=outer();return [await i.next(),await i.return(Promise.resolve(8)),log];}"
  ],
  [
    "vm_stage7_delegate_throw",
    "async function main(){async function* inner(){try{yield 1;}catch(e){yield e;return 8;}}async function* outer(){return yield* inner();}const i=outer();return [await i.next(),await i.throw(7),await i.next()];}"
  ],
  [
    "vm_stage7_reentrant",
    "async function main(){let i,p;async function* g(){p=i.next();yield 1;yield 2;}i=g();const a=await i.next();return [a,await p,await i.next()];}"
  ],
  [
    "vm_stage7_delegated_promise_identity",
    "async function main(){const p=Promise.resolve(7);const o={[Symbol.asyncIterator](){return {next(){return {value:p,done:false};}};}};async function* g(){yield* o;}return (await g().next()).value===p;}"
  ],
  [
    "vm_stage7_thenable_yield",
    "async function main(){async function* g(){yield {then(resolve,reject){resolve(7);reject(8);throw 9;}};}return (await g().next()).value;}"
  ],
  [
    "vm_stage7_then_getter_throw",
    "async function main(){const e={};async function* g(){try{yield {get then(){throw e;}};}catch(x){yield x===e;}}return (await g().next()).value;}"
  ],
  [
    "vm_stage7_promise_constructor_getter",
    "async function main(){const p=Promise.resolve(7);Object.defineProperty(p,'constructor',{get(){throw new Error('broken');}});async function* g(){yield 1;}try{await g().return(p);}catch(e){return e.message;}}"
  ],
  [
    "vm_stage7_bare_return_ticks",
    "async function main(){const log=[];const ticks=Promise.resolve().then(()=>log.push('t1')).then(()=>log.push('t2'));async function* a(){}async function* b(){return;}async function* c(){return undefined;}await Promise.all([ticks,a().next().then(()=>log.push('a')),b().next().then(()=>log.push('b')),c().next().then(()=>log.push('c'))]);return log;}"
  ],
  [
    "vm_stage7_for_await_ticks",
    "async function main(){const log=[];const ticks=Promise.resolve().then(()=>log.push('t1')).then(()=>log.push('t2'));async function f(){const o={[Symbol.asyncIterator](){return [1][Symbol.iterator]();}};log.push('pre');for await(const x of o){log.push('body');}log.push('post');}await Promise.all([ticks,f()]);return log;}"
  ],
  [
    "vm_stage7_throw_promise_identity",
    "async function main(){const e=Promise.resolve(7);async function* g(){try{yield 1;}catch(x){yield x===e;}}const i=g();await i.next();return (await i.throw(e)).value;}"
  ],
  [
    "vm_stage7_parameter_error_sync",
    "async function main(){let caught=false;function fail(){throw 7;}async function* g(x=fail()){yield x;}try{g();}catch(e){caught=e===7;}return caught;}"
  ],
  [
    "vm_stage7_invalid_next_result",
    "async function main(){let closed=0;const o={[Symbol.asyncIterator](){return {next(){return 7;},return(){closed++;return {};}};}};try{for await(const x of o){}}catch(e){return [e instanceof TypeError,closed];}}"
  ],
  [
    "vm_stage7_done_getter_failure",
    "async function main(){let closed=0;const o={[Symbol.asyncIterator](){return {next(){return {get done(){throw 7;}};},return(){closed++;return {};}};}};try{for await(const x of o){}}catch(e){return [e,closed];}}"
  ],
  [
    "vm_stage7_value_getter_failure",
    "async function main(){let closed=0;const o={[Symbol.asyncIterator](){return {next(){return {done:false,get value(){throw 7;}};},return(){closed++;return {};}};}};try{for await(const x of o){}}catch(e){return [e,closed];}}"
  ],
  [
    "vm_stage7_delegate_missing_throw_closes",
    "async function main(){const log=[];const o={[Symbol.asyncIterator](){return {next(){return {value:1,done:false};},async return(){await 0;log.push('closed');return {};}};}};async function* g(){yield* o;}const i=g();await i.next();try{await i.throw(7);}catch(e){return [e instanceof TypeError,log];}}"
  ],
  [
    "vm_stage7_sync_brand_rejects_async",
    "async function main(){async function* a(){}function* b(){}let one=false,two=false;try{b().next.call(a());}catch(e){one=e instanceof TypeError;}try{await a().next.call(b());}catch(e){two=e instanceof TypeError;}return [one,two];}"
  ],
  [
    "vm_stage7_resolving_function_shape",
    "async function main(){let resolve,reject;async function* g(){yield {then(a,b){resolve=a;reject=b;a(7);}};}await g().next();return [resolve.name,resolve.length,reject.name,reject.length];}"
  ],
  [
    "vm_stage7_sync_fallback_ticks",
    "async function main(){const log=[];const ticks=Promise.resolve().then(()=>log.push('t1')).then(()=>log.push('t2')).then(()=>log.push('t3')).then(()=>log.push('t4'));async function f(){log.push('pre');for await(const x of [Promise.resolve(1)])log.push('body');log.push('post');}await Promise.all([ticks,f()]);return log;}"
  ],
  [
    "vm_stage7_queued_values_gc",
    "async function main(){async function* g(){yield 1;const x=yield 2;for(let n=0;n<1800;n++){const garbage=[n];}yield x.n;}const i=g(),a=i.next(),b=i.next(),c=i.next({n:7});return [await a,await b,await c];}"
  ],
  [
    "vm_stage7_generator_callback_is_not_async_callback",
    "async function main(){const out=[];[1,2].forEach(async function* (x){out.push(x);yield x;});const a=[7].map(async function* (x){yield x;});return [out,(await a[0].next()).value];}"
  ]
];
Promise.all(cases.map(async([name,source])=>{try{return {name,result:await Function('"use strict";'+source+';return main();')()};}catch(e){return {name,error:String(e)};}})).then(rows=>console.log(JSON.stringify(rows,null,2)));
