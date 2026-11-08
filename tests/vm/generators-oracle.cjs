// Pure standard-JS counterparts of tests_stage6.rs; no host operations.
const cases=[
  [
    "vm_stage6_lazy_body_and_next_values",
    "function main(){let n=0;function* g(){n++;const x=yield 1;yield x+2;return 9;}const it=g(),before=n;return [before,it.next(999),n,it.next(5),it.next(),it.next(7)];}"
  ],
  [
    "vm_stage6_bare_yield",
    "function main(){function* g(){yield;return yield 2;}const i=g();return [i.next(),i.next(),i.next(7)];}"
  ],
  [
    "vm_stage6_parameters_initialize_at_call",
    "function main(){let n=0;function* g(x=++n){yield x;}const a=g(),b=g();let bad=false;function fail(){throw new ReferenceError('parameter');}function* h(x=fail()){yield x;}try{h();}catch(e){bad=e instanceof ReferenceError;}return [n,a.next().value,b.next().value,bad];}"
  ],
  [
    "vm_stage6_expression_methods_this_and_closure",
    "function main(){const o={n:7,*g(x){yield this.n+x;}},f=function* named(){yield named===f;};class C{*g(){yield 3;}static *g(){yield 4;}}return [o.g(2).next().value,f().next().value,new C().g().next().value,C.g().next().value];}"
  ],
  [
    "vm_stage6_throw_is_catchable_and_preserves_identity",
    "function main(){const e={n:7};function* g(){try{yield 1;}catch(x){yield x===e;}return 3;}const i=g();return [i.next(),i.throw(e),i.next()];}"
  ],
  [
    "vm_stage6_throw_before_start_and_after_completion",
    "function main(){let n=0;function* g(){try{n++;yield 1;}finally{n++;}}const i=g(),e={};let same=0;try{i.throw(e);}catch(x){if(x===e)same++;}try{i.throw(e);}catch(x){if(x===e)same++;}return [n,same,i.next(),i.return(8)];}"
  ],
  [
    "vm_stage6_return_before_start",
    "function main(){let n=0;function* g(){try{n++;yield 1;}finally{n++;}}const i=g();return [i.return(7),i.next(),n];}"
  ],
  [
    "vm_stage6_return_runs_finally_without_catch",
    "function main(){const log=[];function* g(){try{try{yield 1;}catch(e){log.push('catch');}}finally{log.push('finally');}}const i=g();i.next();return [i.return(7),log,i.next()];}"
  ],
  [
    "vm_stage6_return_pending_across_finally_yields",
    "function main(){function* g(){try{yield 1;}finally{yield 2;yield 3;}}const i=g();return [i.next(),i.return(9),i.next(55),i.next(66),i.next()];}"
  ],
  [
    "vm_stage6_finally_can_override_return_and_throw",
    "function main(){function* g(){try{yield 1;}finally{yield 2;return 8;}}const i=g();i.next();const a=i.throw(7),b=i.next();function* h(){try{yield 1;}finally{throw 9;}}const j=h();j.next();let caught;try{j.return(2);}catch(e){caught=e;}return [a,b,caught,j.next()];}"
  ],
  [
    "vm_stage6_reentrant_resume_is_type_error",
    "function main(){let i;const out=[];function* g(){for(const name of ['next','return','throw']){try{i[name](1);}catch(e){out.push(e instanceof TypeError);}}yield 7;}i=g();return [i.next().value,out];}"
  ],
  [
    "vm_stage6_protocol_consumers_and_close",
    "function main(){const log=[];function* g(){try{yield 1;yield 2;yield 3;}finally{log.push('close');}}const a=[...g()],b=Array.from(g(),x=>x*2);for(const x of g())break;const [v]=g();return [a,b,v,log];}"
  ],
  [
    "vm_stage6_interleaved_instances",
    "function main(){function* g(n){let v=n;while(true)v+=yield v;}const a=g(1),b=g(10);return [a.next().value,b.next().value,a.next(2).value,b.next(4).value,a.next(3).value];}"
  ],
  [
    "vm_stage6_generator_function_prototype_and_brand",
    "function main(){function* g(a,b){}const i=g(),p=Object.getPrototypeOf(g),gp=Object.getPrototypeOf(g.prototype);let bad=0;for(const name of ['next','return','throw'])try{i[name].call({});}catch(e){if(e instanceof TypeError)bad++;}try{new g();}catch(e){if(e instanceof TypeError)bad++;}return [g.name,g.length,Object.getPrototypeOf(i)===g.prototype,Object.getPrototypeOf(p)===Function.prototype,p.prototype===gp,i[Symbol.iterator]()===i,Object.prototype.toString.call(i),Object.prototype.toString.call(g),Object.keys(i),bad];}"
  ],
  [
    "vm_stage6_changed_generator_prototype",
    "function main(){function* g(){yield 7;}const old=g.prototype,a=g();g.prototype={custom:3};const b=g();g.prototype=null;const c=g();return [Object.getPrototypeOf(a)===old,Object.getPrototypeOf(b)===g.prototype,b.custom,typeof b.next,c.next().value];}"
  ],
  [
    "vm_stage6_yield_star_nested_and_completion",
    "function main(){function* inner(){yield 2;return 8;}function* g(){yield 1;const x=yield* inner();yield x;return yield* [3,4];}const i=g();return [i.next(),i.next(),i.next(),i.next(),i.next(),i.next()];}"
  ],
  [
    "vm_stage6_yield_star_throw_and_return_forwarding",
    "function main(){const calls=[];const source={[Symbol.iterator](){return this;},next(x){calls.push(['next',x]);return {value:1,done:false};},throw(x){calls.push(['throw',x]);return {value:2,done:false};},return(x){calls.push(['return',x]);return {value:x+1,done:true};}};function* g(){try{return yield* source;}finally{calls.push(['finally']);}}const i=g();return [i.next(),i.next(4),i.throw(5),i.return(7),calls];}"
  ],
  [
    "vm_stage6_yield_star_missing_throw_closes",
    "function main(){let closed=0;const source={[Symbol.iterator](){return this;},next(){return {value:1,done:false};},return(){closed++;return {};}};function* g(){yield* source;}const i=g();i.next();let bad=false;try{i.throw(7);}catch(e){bad=e instanceof TypeError;}return [closed,bad,i.next()];}"
  ],
  [
    "vm_stage6_yield_star_preserves_result_identity_and_done_getter",
    "function main(){let reads=0;const step={value:7,get done(){reads++;return false;}},source={[Symbol.iterator](){return this;},next(){return step;}};function* g(){yield* source;}const i=g(),result=i.next();return [result===step,reads,result.value];}"
  ],
  [
    "vm_stage6_yield_star_return_done_false_resumes_normally",
    "function main(){const calls=[];const source={[Symbol.iterator](){return this;},next(x){calls.push(['next',x]);return {value:2,done:calls.length>2};},return(x){calls.push(['return',x]);return {value:3,done:false};}};function* g(){const v=yield* source;return v+10;}const i=g();return [i.next(),i.return(9),i.next(4),calls];}"
  ],
  [
    "vm_stage6_yield_star_non_object_result_throws",
    "function main(){function* g(){try{yield* {[Symbol.iterator](){return {next(){return 1;}};}};}catch(e){yield e instanceof TypeError;}}return g().next().value;}"
  ],
  [
    "vm_stage6_generator_method_super",
    "function main(){class A{m(){return this.n;}}class B extends A{constructor(){super();this.n=7;}*g(){yield super.m();}}return new B().g().next().value;}"
  ],
  [
    "vm_stage6_generator_descriptors",
    "function main(){function* g(){}const fp=Object.getPrototypeOf(g),gp=Object.getPrototypeOf(g.prototype),out=[];for(const [obj,key] of [[g,'prototype'],[g,'name'],[g,'length'],[fp,'prototype'],[fp,'constructor'],[gp,'constructor'],[gp,'next'],[gp,'return'],[gp,'throw']]){const d=Object.getOwnPropertyDescriptor(obj,key);out.push([d.writable,d.enumerable,d.configurable]);}return out;}"
  ],
  [
    "vm_stage6_return_replaces_pending_return",
    "function main(){const log=[];function* g(){try{try{yield 1;}finally{yield 2;log.push('inner');}}finally{log.push('outer');yield 3;}}const i=g();return [i.next(),i.return(7),i.return(8),i.next(99),log];}"
  ],
  [
    "vm_stage6_yield_star_missing_return_finishes_outer",
    "function main(){const log=[];function* g(){try{yield* {[Symbol.iterator](){return this;},next(){return {value:1,done:false};}};log.push('after');}finally{log.push('finally');yield 2;}}const i=g();return [i.next(),i.return(7),i.next(),log];}"
  ],
  [
    "vm_stage6_yield_star_cached_next_and_lazy_value",
    "function main(){let gets=0,values=0;const step={done:false,get value(){values++;return 3;}},source={[Symbol.iterator](){return this;},get next(){gets++;return function(){return step;};}};function* g(){yield* source;}const i=g(),a=i.next(),b=i.next(7);return [a===step,b===step,gets,values];}"
  ],
  [
    "vm_stage6_yield_star_close_error_precedence",
    "function main(){const error={};const source={[Symbol.iterator](){return this;},next(){return {done:false};},return(){throw error;}};function* g(){yield* source;}const i=g();i.next();let same=false;try{i.throw(7);}catch(e){same=e===error;}return [same,i.next()];}"
  ],
  [
    "vm_stage6_completed_generators_stay_extensible",
    "function main(){function* g(){yield 1;return 2;}const i=g();i.x=7;Object.freeze(i);const out=[i.next(),i.next(),i.next(),i.x,Object.isFrozen(i)];let bad=false;try{structuredClone(i);}catch(e){bad=true;}return [out,bad];}"
  ],
  [
    "vm_stage6_generator_getter_and_promise_callbacks",
    "async function main(){const o={get value(){return (function*(){yield 7;})();}};function* g(){yield 8;}const i=await Promise.resolve().then(g);return [o.value.next().value,i.next().value];}"
  ],
  [
    "vm_stage6_instance_prototype_read_after_parameter_initialization",
    "function main(){function* g(x=(g.prototype={n:7})){yield x;}const old=g.prototype,i=g();return [Object.getPrototypeOf(i)!==old,Object.getPrototypeOf(i)===g.prototype,i.n,i[Symbol.iterator]===undefined];}"
  ],
  [
    "vm_stage6_yield_accessors_and_coercion",
    "function main(){let gets=0,converts=0;const o={get n(){gets++;return 7;}},v={valueOf(){converts++;return 3;}};function* g(){yield o.n;yield v+2;}const i=g();return [i.next(),i.next(),gets,converts];}"
  ],
  [
    "vm_stage6_destructuring_elisions_advance_generator",
    "function main(){let first=0,second=0;function* g(){first++;yield;second++;}const skip=([,])=>0;skip(g());return [first,second];}"
  ],
  [
    "vm_stage6_destructuring_elision_propagates_generator_error",
    "function main(){const error={};function* g(){throw error;}let same=false;try{(([,])=>{})(g());}catch(e){same=e===error;}return same;}"
  ],
  [
    "vm_stage6_elision_shape_preserves_comments_defaults_and_super",
    "function main(){let n=0;function* g(){n++;yield; n++;yield 2; n++;yield 3;}const f=([x=',' /* ,,] */, , ,])=>x,a=f(g()),before=n;n=0;const h=([x,])=>x;h(g());const trailing=n;class A{m(){return 7;}}class B extends A{m(){const pick=([x=super.m(),,])=>x;return pick(g());}}return [a,before,trailing,new B().m()];}"
  ]
];
Promise.all(cases.map(async ([name,source])=>{try{return {name,result:await Function('"use strict";'+source+';return main();')()};}catch(e){return {name,error:e.stack};}})).then(rows=>console.log(JSON.stringify(rows,null,2)));
