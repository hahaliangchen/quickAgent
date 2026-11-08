use super::*;
use runtime::ActionKind;
#[path="tests_stage3c.rs"]
mod stage3c;
#[path="tests_stage4.rs"]
mod stage4;
#[path = "tests_stage5.rs"]
mod stage5;
#[path="tests_stage5_fixes.rs"]
mod stage5_fixes;
#[path = "tests_stage6.rs"]
mod stage6;
#[path="tests_stage7.rs"]
mod stage7;

#[test]
fn vm_stage3b_array_json_reads_inherited_indices() {
    assert_eq!(returned(runtime("function main(){Array.prototype[0]=7;const a=Array(2);a[1]=2;return [JSON.stringify(a),a];}")),json!(["[7,2]",[7,2]]));
}

#[test]
fn vm_stage3b_native_constructor_aliases_and_bound_promise() {
    assert_eq!(returned(runtime("async function main(){const P=Promise,Bound=P.bind(null,resolve=>resolve(7));const p=new Bound();const M=Map,R=RegExp;const m=new M([['x',2]]),re=new R('x','i'),resolve=P.resolve.bind(P);return [await p,p instanceof P,Object.getPrototypeOf(p)===P.prototype,m.get('x'),re.test('X'),await resolve(8)];}")),json!([7,true,true,2,true,8]));
}

#[test]
fn vm_stage3b_object_literal_prototype_and_json_proto_key_are_distinct() {
    assert_eq!(returned(runtime("function main(){const p={n:7},o={__proto__:p,own:2},nil={__proto__:null},normal={__proto__:1};const data=JSON.parse('{\"__proto__\":{\"n\":3}}');return [o.n,Object.getPrototypeOf(o)===p,Object.keys(o),Object.getPrototypeOf(nil)===null,typeof nil.toString,Object.getPrototypeOf(normal)===Object.prototype,Object.hasOwn(data,'__proto__'),Object.getPrototypeOf(data)===Object.prototype];}")),json!([7,true,["own"],true,"undefined",true,true,true]));
}

#[test]
fn vm_stage3b_inherited_properties_shadow_delete_and_original_receiver() {
    assert_eq!(returned(runtime("function main(){const root={n:3,read(){return this.n;},missing:undefined},mid=Object.create(root),obj=Object.create(mid);obj.n=7;const before=[obj.read(),Object.hasOwn(obj,'read'),'read' in obj,'missing' in obj];obj.read=function(){return this.n+1;};const own=obj.read();delete obj.read;return [before,own,obj.read(),Object.getPrototypeOf(obj)===mid,Object.getPrototypeOf(mid)===root];}")),json!([[7,false,true,true],8,7,true,true]));
}

#[test]
fn vm_stage3b_null_prototype_has_no_implicit_object_methods() {
    assert_eq!(returned(runtime("function main(){const o=Object.create(null);o.x=1;return [Object.getPrototypeOf(o)===null,typeof o.toString,typeof o.constructor,'toString' in o,o instanceof Object,Object.keys(o),JSON.stringify(o)];}")),json!([true,"undefined","undefined",false,false,["x"],"{\"x\":1}"]));
}

#[test]
fn vm_stage3b_set_prototype_rejects_cycles_and_invalid_inputs() {
    assert_eq!(returned(runtime("function main(){let count=0;const a={},b=Object.create(a);for(const pair of [[a,a],[a,b],[{},1],[null,{}],[undefined,null]]){try{Object.setPrototypeOf(pair[0],pair[1]);}catch(e){if(e instanceof TypeError)count++;}}for(const proto of [undefined,1,'x',true]){try{Object.create(proto);}catch(e){if(e instanceof TypeError)count++;}}return [count,Object.setPrototypeOf(1,{})===1,Object.setPrototypeOf('x',null)==='x',Object.getPrototypeOf(b)===a];}")),json!([9,true,true,true]));
}

#[test]
fn vm_stage3b_frozen_and_immutable_prototypes() {
    assert_eq!(returned(runtime("function main(){const p={},o=Object.freeze(Object.create(p));let failed=0;try{Object.setPrototypeOf(o,{});}catch(e){if(e instanceof TypeError)failed++;}try{Object.setPrototypeOf(Object.prototype,{});}catch(e){if(e instanceof TypeError)failed++;}return [Object.setPrototypeOf(o,p)===o,Object.setPrototypeOf(Object.prototype,null)===Object.prototype,failed];}")),json!([true,true,2]));
}

#[test]
fn vm_stage3b_intrinsic_prototype_hierarchy_and_callable_function_prototype() {
    assert_eq!(returned(runtime("function main(){return [Object.getPrototypeOf({})===Object.prototype,Object.getPrototypeOf([])===Array.prototype,Object.getPrototypeOf(Object)===Function.prototype,Object.getPrototypeOf(Function.prototype)===Object.prototype,Object.getPrototypeOf(Object.prototype)===null,typeof Function.prototype,Function.prototype()===undefined,Array.isArray(Array.prototype),Object.keys(Object.prototype),Object.keys(Array.prototype),JSON.stringify(Object.prototype)];}")),json!([true,true,true,true,true,"function",true,true,[],[],"{}"]));
}

#[test]
fn vm_stage3b_function_prototypes_new_and_bound_instanceof() {
    assert_eq!(returned(runtime("function main(){function C(n){this.n=n;}C.prototype.read=function(){return this.n;};const Bound=C.bind({n:99},7),a=new Bound(),b=Object.create(C.prototype);return [a.n,a.read(),a instanceof C,a instanceof Bound,b instanceof C,C.prototype.constructor===C,Object.hasOwn(C,'prototype'),Object.hasOwn(Bound,'prototype'),Object.hasOwn(()=>{},'prototype'),Object.keys(C),Object.keys(C.prototype)];}")),json!([7,7,true,true,true,true,true,false,false,[],["read"]]));
}

#[test]
fn vm_stage3b_constructor_object_return_and_invalid_constructor() {
    assert_eq!(returned(runtime("function main(){function A(){this.n=1;return 7;}function B(){return {n:2};}let failed=0;for(const fn of [()=>{},async function(){},({m(){}}).m]){try{new fn();}catch(e){if(e instanceof TypeError)failed++;}}return [new A().n,new B().n,failed];}")),json!([1,2,3]));
}

#[test]
fn vm_stage3b_instanceof_uses_actual_chain_and_runtime_rhs() {
    assert_eq!(returned(runtime("function main(){function C(){}const old=C.prototype,a=Object.create(old);C.prototype={};const b=Object.create(C.prototype),rhs={Ctor:C};const before=[a instanceof C,b instanceof rhs.Ctor];let bad=0;try{a instanceof {};}catch(e){if(e instanceof TypeError)bad++;}C.prototype=1;try{a instanceof C;}catch(e){if(e instanceof TypeError)bad++;}return [before,a instanceof Object,bad,1 instanceof C];}")),json!([[false,true],true,2,false]));
}

#[test]
fn vm_stage3b_builtin_instanceof_tracks_changes_without_granting_internal_slots() {
    assert_eq!(returned(runtime("function main(){const fake=Object.create(Map.prototype),m=new Map();let bad=false;try{Map.prototype.get.call(fake,'x');}catch(e){bad=e instanceof TypeError;}const before=[fake instanceof Map,m instanceof Map,bad];Object.setPrototypeOf(m,null);return [before,m instanceof Map,m instanceof Object,typeof m.get,typeof m.size];}")),json!([[true,true,true],false,false,"undefined","undefined"]));
}

#[test]
fn vm_stage3b_inherited_enumeration_keeps_own_json_and_non_enumerable_intrinsics() {
    assert_eq!(returned(runtime("function main(){const p={a:1,b:2},o=Object.create(p);o.a=3;o.c=4;const names=[];for(const k in o)names.push(k);const methods=[];for(const k in [])methods.push(k);return [names,Object.keys(o),Object.values(o),JSON.stringify(o),Object.assign({},o),methods];}")),json!([["a","c","b"],["a","c"],[3,4],"{\"a\":3,\"c\":4}",{"a":3,"c":4},[]]));
}

#[test]
fn vm_stage3b_prototype_method_changes_are_visible_and_overrides_win() {
    assert_eq!(returned(runtime("function main(){const a=[1,2],native=Array.prototype.join;Array.prototype.join=function(){return this.length+' items';};const changed=a.join();a.join=function(){return 'own';};const own=a.join();delete a.join;delete Array.prototype.join;return [changed,own,typeof a.join,typeof native,typeof Object.create(null).valueOf];}")),json!(["2 items","own","undefined","function","undefined"]));
}

#[test]
fn vm_stage3b_array_holes_read_and_map_inherited_indices() {
    assert_eq!(returned(runtime("function main(){Array.prototype[0]=7;const a=Array(2);a[1]=2;const result=[a[0],0 in a,Object.hasOwn(a,0),a.map(x=>x*2)];delete Array.prototype[0];return result;}")),json!([7,true,false,[14,4]]));
}

#[test]
fn vm_stage3b_standard_function_prototype_call_apply_bind_and_static_values() {
    assert_eq!(returned(runtime("function main(){function f(a){return this.n+a;}const o={n:3};const call=Function.prototype.call,apply=Function.prototype.apply;const a=call.call(f,o,4),b=apply.call(f,o,[5]),bound=Function.prototype.bind.call(f,o,6);Object.getPrototypeOf=function(){return 'changed';};return [a,b,bound(),Object.getPrototypeOf(o),Object.prototype.hasOwnProperty.call(o,'n')];}")),json!([7,8,9,"changed",true]));
}

#[test]
fn vm_stage3b_primitives_box_to_intrinsic_prototypes() {
    assert_eq!(returned(runtime("function main(){const n=Object(3),s=Object('hi');return [Object.getPrototypeOf(3)===Number.prototype,Object.getPrototypeOf('x')===String.prototype,Object.getPrototypeOf(false)===Boolean.prototype,n.valueOf(),s.valueOf(),Object.getPrototypeOf(n)===Number.prototype,Object.getPrototypeOf(s)===String.prototype];}")),json!([true,true,true,3,"hi",true,true]));
}

#[test]
fn vm_stage3b_prototype_only_references_survive_gc_and_snapshot() {
    let mut run=runtime("function main(){function make(){const ancestor={n:7,read(){return this.n;}};return Object.create(ancestor);}const o=make();for(let i=0;i<400;i++){const garbage=[i];}io.read({});return [o.read(),Object.getPrototypeOf(o).n];}");
    assert!(matches!(run.next(),State::Action(_)));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}),false).unwrap();
    assert_eq!(returned(run),json!([7,7]));
}

#[test]
fn vm_stage3b_constructor_suspend_restore_preserves_instance_and_return_rules() {
    let mut run=runtime("function main(){function C(n){this.n=n;const value=io.read({});this.n+=value.n;}C.prototype.read=function(){return this.n;};const a=new C(7);return [a.read(),a instanceof C,Object.getPrototypeOf(a)===C.prototype];}");
    assert!(matches!(run.next(),State::Action(_)));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":2}),false).unwrap();
    assert_eq!(returned(run),json!([9,true,true]));
}

#[test]
fn vm_stage3b_inherited_array_indices_are_visible_to_all_builtin_iterators() {
    assert_eq!(returned(runtime("function main(){Array.prototype[0]=7;const a=Array(2);a[1]=2;const values=[...a],from=Array.from(a),loop=[];for(const v of a)loop.push(v);delete Array.prototype[0];return [values,from,loop,Array.from({0:3,length:1},v=>v+1)];}")),json!([[7,2],[7,2],[7,2],[4]]));
}

#[test]
fn vm_stage3b_bound_function_uses_target_internal_prototype_and_inherited_methods() {
    let mut run=runtime("function main(){const custom=Object.create(Function.prototype);custom.tag=7;function f(){return this.n;}Object.setPrototypeOf(f,custom);const bound=f.bind({n:3});io.read({});return [Object.getPrototypeOf(bound)===custom,bound.tag,bound(),Object.prototype.isPrototypeOf.call(Function.prototype,bound)];}");
    assert!(matches!(run.next(),State::Action(_)));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}),false).unwrap();
    assert_eq!(returned(run),json!([true,7,3,true]));
}

#[test]
fn vm_stage3b_own_only_json_round_trip_and_readonly_builtin_constructor_prototype() {
    assert_eq!(returned(runtime("function main(){const p={inherited:1},o=Object.create(p);o.own=2;const clone=structuredClone(o);let failed=false;try{Array.prototype={};}catch(e){failed=e instanceof TypeError;}return [Object.getPrototypeOf(clone)===Object.prototype,Object.keys(clone),clone.inherited,failed,JSON.stringify(Object(3)),JSON.stringify(Object('x'))];}")),json!([true,["own"],null,true,"3","\"x\""]));
}

#[test]
fn vm_stage3b_inherited_async_method_keeps_receiver_through_await() {
    assert_eq!(returned(runtime("async function main(){const proto={async read(){await 0;return this.n;}},o=Object.create(proto);o.n=7;return await o.read();}")),json!(7));
}

#[test]
fn vm_stage3a_strict_this_and_method_receivers() {
    assert_eq!(returned(runtime("function main(){function receiver(){return this;}const a={n:7,receiver},b={n:9,receiver};const plain=a.receiver;return [this===undefined,receiver()===undefined,plain()===undefined,a.receiver()===a,b['receiver']()===b,(a.receiver)()===a,receiver.call(null)===null,receiver.call(3)===3,receiver.call('x')==='x'];}")),json!([true,true,true,true,true,true,true,true,true]));
    let mut top=runtime("console.log(this===undefined);");
    assert!(matches!(top.next(),State::Returned(_)));
    assert_eq!(top.logs(),["true"]);
}

#[test]
fn vm_stage3a_arrow_this_is_lexical_including_parameters_and_blocks() {
    assert_eq!(returned(runtime("function main(){function make(arg=this){const arrow=(x=this)=>[this,arg,x];return {arrow,ordinary:function(){return this;}};}const obj={make};const made=obj.make();return [made.arrow.call(8)[0]===obj,made.arrow.bind(9)()[1]===obj,made.arrow()[2]===obj,made.ordinary()===made,(()=>this).call(obj)===undefined];}")),json!([true,true,true,true,true]));
}

#[test]
fn vm_stage3a_call_apply_bind_and_rebinding_argument_order() {
    assert_eq!(returned(runtime("function main(){function sum(a,b,c){return [this.n,a,b,c];}const first={n:7},other={n:8};const bound=sum.bind(first,1),twice=bound.bind(other,2);return [sum.call(first,1,2,3),sum.apply(first,{0:1,1:2,2:3,length:3}),sum.apply(first,null),twice(3),twice.call(other,4),bound!==sum,bound instanceof Function,typeof bound,JSON.stringify({fn:bound})];}")),json!([[7,1,2,3],[7,1,2,3],[7,null,null,null],[7,1,2,3],[7,1,2,4],true,true,"function","{}"]));
}

#[test]
fn vm_stage3a_bind_name_length_and_parameter_length() {
    assert_eq!(returned(runtime("function main(){function sum(a,b=1,c){}function rest(a,...tail){}const bound=sum.bind(null,3),again=bound.bind(null);return [sum.name,sum.length,rest.length,bound.name,bound.length,again.name,(()=>{}).length,String.length,[].map.length,Math.max.bind(null,1).length,Array.from.length,setTimeout.length];}")),json!(["sum",1,1,"bound sum",0,"bound bound sum",0,1,1,1,1,2]));
}

#[test]
fn vm_stage3a_method_is_read_before_argument_side_effects() {
    assert_eq!(returned(runtime("function main(){const obj={n:7,f(x){return this.n+x;}};function change(){obj.f=()=>99;return 2;}const a=obj.f(change());obj.f=function(x){return this.n+x;};const b=obj['f'](...[change()]);return [a,b];}")),json!([9,9]));
}

#[test]
fn vm_stage3a_optional_calls_preserve_reference_and_short_circuit_arguments() {
    assert_eq!(returned(runtime("function main(){let calls=0;const o={n:7,f(...args){return [this.n,...args];}};const a=o.f?.(1),b=o['f']?.(...[2]),c=o?.['f'](3);const nil=null;const d=nil?.f(++calls),e=o.missing?.(++calls);return [a,b,c,d,e,calls];}")),json!([[7,1],[7,2],[7,3],null,null,0]));
}

#[test]
fn vm_stage3a_parentheses_and_expression_callees_keep_js_reference_rules() {
    assert_eq!(returned(runtime("function main(){const o={f(){return this;}};function factory(){return o.f;}let f=()=>1;f=f.bind(null);return [(o.f)()===o,(o.f)?.()===o,(o.f||null)()===undefined,factory()()===undefined,[o.f][0]() instanceof Array,f?.(),factory()?.()===undefined];}")),json!([true,true,true,true,true,1,true]));
}

#[test]
fn vm_stage3a_parenthesized_optional_reference_keeps_receiver_but_not_outer_short_circuit() {
    assert_eq!(returned(runtime("function main(){let count=0;const o={n:7,f(x){return this.n+x;}},nil=null;const a=(o?.f)(2),b=(o?.f)(...[3]),c=(o?.f)?.(4);let failed=false;try{(nil?.f)(++count);}catch(e){failed=e instanceof TypeError;}return [a,b,c,count,failed];}")),json!([9,10,11,1,true]));
}

#[test]
fn vm_stage3a_bound_function_survives_host_suspend_gc_and_restore() {
    let mut run=runtime("function main(){function create(){const target={n:7};return function(x){const v=io.read({path:'x'});this.n+=v.n;return [this.n,x];}.bind(target,3);}const fn=create();for(let i=0;i<400;i++){const garbage=[i];}return fn();}");
    assert!(matches!(run.next(),State::Action(_)));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":2}),false).unwrap();
    assert_eq!(returned(run),json!([9,3]));
}

#[test]
fn vm_stage3a_bound_async_promise_and_executor_callbacks() {
    assert_eq!(returned(runtime("async function main(){const obj={n:7,f:async function(x){await 0;return this.n+x;}};const bound=obj.f.bind(obj,2);const a=await bound();const b=await Promise.resolve(3).then(obj.f.bind(obj));const c=await new Promise(function(resolve){resolve(this.n);}.bind(obj));return [a,b,c];}")),json!([9,10,7]));
}

#[test]
fn vm_stage3a_callbacks_honor_this_arg_and_arrow_ignores_it() {
    assert_eq!(returned(runtime("function main(){const context={n:3};const mapped=[1,2].map(function(v){return this.n+v;},context);let sum=0;new Set([1,2]).forEach(function(v){sum+=this.n+v;},context);const from=Array.from(new Set([2,4]),function(v){return this.n+v;},context);return [mapped,sum,from,[1].map(()=>this===undefined,context),[1,2].filter(function(v){return this.n>v;},context)];}")),json!([[4,5],9,[5,7],[true],[1,2]]));
}

#[test]
fn vm_stage3a_native_tools_collections_and_extracted_call_apply() {
    let mut run=runtime("function main(){function receiver(){return this;}const invoke=receiver.call;const a=invoke.call(receiver,7);const b=Math.max.bind(null,1)(2,3);const m=new Map([['x',4]]);const get=m.get.bind(m);const c=get('x');const d=[1,2].slice.call([3,4],1);const read=io.read.bind(null,{path:'x'});return [a,b,c,d,read().n];}");
    assert!(matches!(run.next(),State::Action(_)));
    run.resume(json!({"n":8}),false).unwrap();
    assert_eq!(returned(run),json!([7,3,4,[4],8]));
}

#[test]
fn vm_stage3a_apply_validation_and_bound_callback_failures_are_type_errors() {
    assert_eq!(returned(runtime("function main(){let count=0;const apply=(function(){}).apply;for(const bad of [1,'x',true]){try{(function(){}).apply(null,bad);}catch(e){if(e instanceof TypeError)count++;}}try{apply.call({},null,[]);}catch(e){if(e instanceof TypeError)count++;}const obj={f:1};let ran=0;try{obj.f(++ran);}catch(e){if(e instanceof TypeError)count++;}return [count,ran];}")),json!([5,1]));
}

#[test]
fn vm_stage3a_bound_and_lexical_timer_callbacks_keep_this_after_snapshot() {
    let mut run=timed_runtime("function main(){const obj={n:7,install(){setTimeout(function(x){console.log(this.n,x);}.bind(this,2),10);setTimeout(()=>console.log(this.n),20);}};obj.install();return true;}");
    assert!(matches!(run.next(),State::Waiting));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.set_test_timer_time(1000);
    let run=drain_timers(run);
    assert_eq!(run.logs(),["7 2","7"]);
    assert_eq!(returned(run),json!(true));
}

#[test]
fn vm_stage3a_parenthesized_spread_and_reassigned_functions_use_current_value() {
    assert_eq!(returned(runtime("function main(){function first(){return 1;}first=function(){return 2;}.bind(null);const obj={n:7,f(...xs){return [this.n,...xs];}};return [first(),(obj.f)(...[1,2]),(obj['f'])(...[3])];}")),json!([2,[7,1,2],[7,3]]));
}

#[test]
fn vm_stage3a_own_methods_with_builtin_names_are_not_replaced_by_optimizations() {
    assert_eq!(returned(runtime("function main(){const obj={n:7,push(...xs){return this.n+xs.length;},slice(...xs){return this.n+xs.length;}};const a=[];a.push=function(v){return this===a?v:0;};return [obj.push(),obj.push(1,2),obj.slice(1,2,3,4),a.push(9)];}")),json!([7,9,11,9]));
}

#[test]
fn vm_stage3a_ordinary_callbacks_receive_undefined_without_this_arg() {
    assert_eq!(returned(runtime("async function main(){const checks=[1].map(function(){return this===undefined;});new Map([[1,2]]).forEach(function(){checks.push(this===undefined);});checks.push(await Promise.resolve(1).then(function(){return this===undefined;}));return checks;}")),json!([true,true,true]));
}

#[test]
fn vm_stage3a_function_properties_override_intrinsics_and_survive_restore() {
    let mut run=runtime("function main(){function f(){}f.call=function(){return this===f;};f.note={n:3};io.read({});const a=f.call();delete f.call;return [a,f.call(null)===undefined,Object.keys(f),f.note.n,f.hasOwnProperty('note'),[].hasOwnProperty('length'),({}).hasOwnProperty('missing')];}");
    assert!(matches!(run.next(),State::Action(_)));
    assert!(!run.named_bindings().keys().any(|name|name.contains('\0')));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}),false).unwrap();
    assert_eq!(returned(run),json!([true,true,["note"],3,true,true,false]));
}

#[test]
fn vm_stage3a_native_optional_calls_and_borrowed_string_methods() {
    assert_eq!(returned(runtime("async function main(){let caught=false;try{await Promise.resolve(1).then((function(){}).call);}catch(e){caught=e instanceof TypeError;}return [Math.max?.(1,3),(Math.max)?.(...[2,4]),Array.from?.(new Set([1,2])),''.slice.call(12345,1,3),caught];}")),json!([3,4,[1,2],"23",true]));
}

fn timed_runtime(source: &str) -> Runtime {
    let mut run=runtime(source);
    run.set_test_timer_time(1_000);
    run
}
fn drain_timers(mut run: Runtime) -> Runtime {
    for _ in 0..100 {
        match run.next() {
            State::Waiting => run.set_test_timer_time(run.next_timer_at().expect("timer wake")),
            State::Returned(_) | State::Failed(_) => return run,
            other => panic!("unexpected host effect: {other:?}"),
        }
    }
    panic!("timers did not finish")
}

#[test]
fn vm_stage2_timer_keeps_root_result_and_arguments_alive() {
    let run=drain_timers(timed_runtime("function main(){const obj={n:7};setTimeout((x,y)=>{console.log(x===obj,y);return 99;},10,obj,'ok');for(let i=0;i<400;i++){const garbage=[i];}return 42;}"));
    assert_eq!(run.logs(),["true ok"]);
    assert_eq!(returned(run),json!(42));
}

#[test]
fn vm_stage2_macro_and_microtasks_share_correct_fifo_checkpoints() {
    let run=drain_timers(timed_runtime("async function main(){console.log('sync');setTimeout(()=>{console.log('timer1');queueMicrotask(()=>console.log('micro-in-timer'));Promise.resolve().then(()=>console.log('promise-in-timer'));},5);setTimeout(()=>console.log('timer2'),5);queueMicrotask(()=>{console.log('micro1');queueMicrotask(()=>console.log('micro2'));});Promise.resolve().then(()=>console.log('promise1'));await 0;console.log('await');return 'done';}"));
    assert_eq!(run.logs(),["sync","micro1","promise1","await","micro2","timer1","micro-in-timer","promise-in-timer","timer2"]);
    assert_eq!(returned(run),json!("done"));
}

#[test]
fn vm_stage2_delay_normalization_and_deadline_order() {
    let mut run=timed_runtime("function main(){for(const delay of [0,-1,NaN,Infinity,2147483648])setTimeout(console.log,delay,'clamped');setTimeout(console.log,'4.9','four');setTimeout(console.log,2,'two');return true;}");
    assert!(matches!(run.next(),State::Waiting));
    assert_eq!(run.next_timer_at(),Some(1001));
    run.set_test_timer_time(2000);
    let run=drain_timers(run);
    assert_eq!(run.logs(),["clamped","clamped","clamped","clamped","clamped","two","four"]);
}

#[test]
fn vm_stage2_clear_handles_are_interchangeable_and_invalid_handles_are_noops() {
    let run=drain_timers(timed_runtime("function main(){const a=setTimeout(()=>console.log('bad'),1),b=setInterval(()=>console.log('bad'),1);clearInterval(a);clearTimeout(b);clearTimeout(undefined);clearInterval({});clearTimeout('garbage');const schedule=setTimeout,cancel=clearTimeout;cancel(String(schedule(()=>console.log('bad'),1)));return typeof a;}"));
    assert!(run.logs().is_empty());
    assert_eq!(returned(run),json!("number"));
}

#[test]
fn vm_stage2_timer_and_microtask_validate_callbacks_synchronously() {
    assert_eq!(returned(timed_runtime("function main(){let count=0;for(const fn of [setTimeout,setInterval,queueMicrotask])for(const cb of [undefined,null,1,'code',{}]){try{fn(cb);}catch(e){if(e instanceof TypeError)count++;}}return count;}")),json!(15));
}

#[test]
fn vm_stage2_timer_and_microtask_errors_are_uncaught() {
    for schedule in ["setTimeout","queueMicrotask"] {
        let run=drain_timers(timed_runtime(&format!("async function main(){{try{{{schedule}(()=>{{throw new TypeError('callback failed');}});}}catch(e){{console.log('wrong catch');}}setTimeout(()=>console.log('must not run'),100);return 7;}}")));
        assert_eq!(run.error.as_ref().unwrap()["name"],"TypeError");
        assert_eq!(run.error.as_ref().unwrap()["message"],"callback failed");
        assert!(run.logs().is_empty());
        assert_eq!(run.next_timer_at(),None);
    }
}

#[test]
fn vm_stage2_interval_restore_fires_once_without_catching_up() {
    let mut run=timed_runtime("function main(){let ticks=0;const id=setInterval(()=>{console.log(++ticks);if(ticks===2)clearTimeout(id);},10);return 'root';}");
    assert!(matches!(run.next(),State::Waiting));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.set_test_timer_time(50_000);
    assert!(matches!(run.next(),State::Waiting));
    assert_eq!(run.logs(),["1"]);
    assert_eq!(run.next_timer_at(),Some(50_010));
    let run=drain_timers(run);
    assert_eq!(run.logs(),["1","2"]);
    assert_eq!(returned(run),json!("root"));
}

#[test]
fn vm_stage2_timeout_snapshot_preserves_absolute_deadline_and_live_closure() {
    let mut run=timed_runtime("function main(){let n=1;const value={n:2};setTimeout(x=>console.log(n,x.n,x===value),500,value);n=3;return true;}");
    assert!(matches!(run.next(),State::Waiting));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.set_test_timer_time(1400);
    assert_eq!(run.next_timer_at(),Some(1500));
    assert!(matches!(run.next(),State::Waiting));
    run.set_test_timer_time(1501);
    let run=drain_timers(run);
    assert_eq!(run.logs(),["3 2 true"]);
}

#[test]
fn vm_stage2_timer_callback_can_suspend_restore_and_resume_without_replay() {
    let mut run=timed_runtime("function main(){setTimeout(()=>{console.log('before');const result=io.read({path:'x'});console.log(result.n);},1);return 9;}");
    assert!(matches!(run.next(),State::Waiting));
    run.set_test_timer_time(1001);
    assert!(matches!(run.next(),State::Action(_)));
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":7}),false).unwrap();
    assert_eq!(returned(run.clone()),json!(9));
    assert!(matches!(run.next(),State::Returned(_)));
    assert_eq!(run.logs(),["before","7"]);
}

#[test]
fn vm_stage2_timer_resolves_pending_promise_and_async_callbacks_do_not_block_ticks() {
    let run=drain_timers(timed_runtime("async function main(){const seen=[];const result=await new Promise(resolve=>{setTimeout(async()=>{seen.push(1);await new Promise(r=>setTimeout(r,10));seen.push(3);resolve('ok');},1);setTimeout(()=>seen.push(2),2);});return [result,seen];}"));
    assert_eq!(returned(run),json!(["ok",[1,2,3]]));
}

#[test]
fn vm_stage2_cancel_clears_durable_callbacks_and_prevents_future_effects() {
    let mut run=timed_runtime("function main(){setInterval(()=>io.read({}),1);return true;}");
    assert!(matches!(run.next(),State::Waiting));
    run.cancel();
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.set_test_timer_time(99999);
    assert_eq!(run.next_timer_at(),None);
    assert!(matches!(run.next(),State::Failed(ref e) if e["code"]=="PROGRAM_CANCELLED"));
}

#[test]
fn vm_stage2_pure_microtask_loop_still_hits_instruction_budget() {
    let mut run=timed_runtime("function main(){function again(){queueMicrotask(again);}queueMicrotask(again);return 1;}");
    assert!(matches!(run.next(),State::Failed(ref e) if e["code"]=="INSTRUCTION_BUDGET_EXHAUSTED"));
}

#[test]
fn vm_stage2_timers_progress_while_parallel_ai_calls_are_inflight() {
    let mut run=timed_runtime("async function main(){const p=worker();await new Promise(r=>setTimeout(()=>{console.log('timer');r();},10));return await p;} /** @virtual */ async function worker(){ /* Return a number. */ }");
    let runtime::ConcurrentState::Actions(actions)=run.next_concurrent() else {panic!("expected concurrent call")};
    assert_eq!(actions.len(),1);
    assert!(matches!(run.next(),State::Waiting));
    run.set_test_timer_time(1010);
    assert!(matches!(run.next(),State::Waiting));
    assert_eq!(run.logs(),["timer"]);
    run.resume_action(&actions[0].id,json!(7),false).unwrap();
    assert_eq!(returned(run),json!(7));
}

#[test]
fn vm_stage2_microtasks_survive_snapshot_and_gc_without_flushing_early() {
    let mut run=timed_runtime("function main(){const obj={n:4};queueMicrotask(()=>console.log(obj.n));io.read({});for(let i=0;i<400;i++){const garbage=[i];}console.log('sync');return 1;}");
    assert!(matches!(run.next(),State::Action(_)));
    assert!(run.logs().is_empty());
    let mut run:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}),false).unwrap();
    assert_eq!(returned(run.clone()),json!(1));
    assert!(matches!(run.next(),State::Returned(_)));
    assert_eq!(run.logs(),["sync","4"]);
}

#[test]
fn vm_stage2_queue_microtask_can_be_a_promise_reaction() {
    let run=drain_timers(timed_runtime("function main(){Promise.resolve(()=>console.log('queued')).then(queueMicrotask);console.log('sync');return 7;}"));
    assert_eq!(run.logs(),["sync","queued"]);
    assert_eq!(returned(run),json!(7));
}

#[test]
fn vm_stage2_scheduled_callbacks_are_gc_roots_after_their_creator_returns() {
    let run=drain_timers(timed_runtime("function main(){function register(){const captured={n:7},arg={n:8};setTimeout(x=>console.log(captured.n,x.n),10,arg);queueMicrotask(()=>console.log(captured.n));}register();for(let i=0;i<400;i++){const garbage=[i];}return 1;}"));
    assert_eq!(run.logs(),["7","7 8"]);
    assert_eq!(returned(run),json!(1));
}

#[test]
fn vm_stage2_timer_refreshes_instruction_segment_but_wall_deadline_still_applies() {
    let mut run=timed_runtime("async function main(){let n=0;for(let i=0;i<3;i++){for(let j=0;j<15000;j++)n++;await new Promise(r=>setTimeout(r,1));}return n;}");
    for _ in 0..3 {
        assert!(matches!(run.next(),State::Waiting));
        run.set_test_timer_time(run.next_timer_at().unwrap());
    }
    assert_eq!(returned(run),json!(45000));
    let mut run=timed_runtime("function main(){setInterval(()=>console.log('tick'),1);return 1;}");
    assert!(matches!(run.next(),State::Waiting));
    let mut snapshot=serde_json::to_value(run).unwrap();
    snapshot["started_at_ms"]=json!(1);
    let mut run:Runtime=serde_json::from_value(snapshot).unwrap();
    assert!(matches!(run.next(),State::Failed(ref e) if e["code"]=="PROGRAM_TIME_BUDGET_EXHAUSTED"));
    assert_eq!(run.next_timer_at(),None);
    assert!(run.logs().is_empty());
}

#[test]
fn vm_stage2_unhandled_async_callback_rejection_stops_before_next_timer() {
    let run=drain_timers(timed_runtime("function main(){setTimeout(async()=>{await 0;throw new Error('async callback');},1);setTimeout(()=>console.log('bad'),2);return 1;}"));
    assert_eq!(run.error.as_ref().unwrap()["message"],"async callback");
    assert!(run.logs().is_empty());
}

#[test]
fn vm_stage1_weak_entries_are_not_exposed_as_array_like_elements() {
    assert_eq!(returned(runtime("function main(){const key={},w=new WeakMap([[key,7]]),s=new WeakSet([key]);const empty=[Array.from(w),Array.from(s)];w.length=2;w[0]='own';return [empty,Array.from(w)];}")),json!([[[],[]],["own",null]]));
}

#[test]
fn vm_stage1_object_from_entries_and_ordinary_numeric_properties() {
    assert_eq!(returned(runtime("function main(){const m=new Map([['a',1],['b',2]]);m[2]='two';m[1]='one';m.note=3;return [Object.fromEntries(m),Object.fromEntries(new Set([{0:'key',1:7}])),Object.keys(m),JSON.stringify(m),m.toString()];}")),
        json!([{"a":1,"b":2},{"key":7},["1","2","note"],"{\"1\":\"one\",\"2\":\"two\",\"note\":3}","[object Map]"]));
}

#[test]
fn vm_stage1_json_only_structured_clone_does_not_silently_erase_collections() {
    assert_eq!(returned(runtime("function main(){let failures=0;for(const value of [new Map(),{nested:new Set()},new Set().values()]){try{structuredClone(value);}catch(e){if(e instanceof TypeError)failures++;}}return failures;}")),json!(3));
}

#[test]
fn vm_stage1_zero_keys_partial_destructuring_and_iterator_only_roots() {
    let mut run=runtime("function main(){const s=new Set([-0]),m=new Map([[-0,1]]);const it=new Map([[{id:7},{n:9}]]).entries();const seq=new Set([10,20,30]).values();const [a,,c]=seq;for(let i=0;i<400;i++){const garbage=[i];}io.read({});const pair=it.next().value;return [1/s.values().next().value===Infinity,1/m.keys().next().value===Infinity,a,c,seq.next().done,pair[0].id,pair[1].n];}");
    assert!(matches!(run.next(),State::Action(_)));
    let mut restored:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    restored.resume(json!({}),false).unwrap();
    assert_eq!(returned(restored),json!([true,true,10,30,true,7,9]));
}

#[test]
fn vm_stage1_promise_combinators_accept_builtin_collections() {
    assert_eq!(returned(runtime("async function main(){const all=await Promise.all(new Set([Promise.resolve(1),Promise.resolve(2)]));const settled=await Promise.allSettled(new Map([[1,2]]));const race=await Promise.race(new Set([3,4]));const any=await Promise.any(new Set([5,6]));let rejected=false;try{await Promise.all(new WeakSet());}catch(e){rejected=e instanceof TypeError;}return [all,settled,race,any,rejected];}")),json!([[1,2],[{"status":"fulfilled","value":[1,2]}],3,5,true]));
}

#[test]
fn vm_stage1_weak_map_keeps_keys_strongly_in_this_implementation() {
    use super::heap::{Heap,VmValue};
    let mut heap=Heap::default();
    let key=heap.from_json(&json!({"id":7})).unwrap();
    let pair=heap.allocate(super::heap::HeapNode::Array(vec![key.clone(),VmValue::String("value".into())])).unwrap();
    let pairs=heap.allocate(super::heap::HeapNode::Array(vec![VmValue::Reference(pair)])).unwrap();
    let weak=super::js_collections::construct(&mut heap,"WeakMap",&[VmValue::Reference(pairs)]).unwrap();
    heap.collect(&[weak.clone()]).unwrap();
    let mut restored:Heap=serde_json::from_value(serde_json::to_value(heap).unwrap()).unwrap();
    assert_eq!(restored.to_json(&key).unwrap(),json!({"id":7}));
    assert_eq!(super::js_collections::call(&mut restored,weak,"get",&[key]).unwrap(),VmValue::String("value".into()));
}

#[test]
fn vm_stage1_map_keys_use_same_value_zero_and_reference_identity() {
    assert_eq!(returned(runtime("function main(){const a={},b={};const m=new Map();return [m.set(a,1)===m,m.set(b,2).set(NaN,3).set(NaN,4).set(-0,5).set(0,6).size,m.get(a),m.get(b),m.get(NaN),m.get(-0),m.has('0'),m.get('missing')===undefined,m.delete(a),m.delete(a),Array.from(m.values())];}")),
        json!([true,4,1,2,4,6,false,true,true,false,[2,4,6]]));
}

#[test]
fn vm_stage1_set_order_constructor_and_builtin_iterables() {
    assert_eq!(returned(runtime("function main(){const s=new Set([3,1,3,2]);s.delete(1);s.add(1);const m=new Map([['a',1],['b',2],['a',3]]);return [s.add(3)===s,s.size,[...s],Array.from(new Set('😀a😀')),Array.from(new Map(m)),[...m.keys()],[...s.entries()],s.keys===s.values,new Map(null).size,new WeakSet(undefined).has(1)];}")),
        json!([true,3,[3,2,1],["😀","a"],[["a",3],["b",2]],["a","b"],[[3,3],[2,2],[1,1]],true,0,false]));
}

#[test]
fn vm_stage1_weak_collections_only_accept_object_keys() {
    assert_eq!(returned(runtime("function main(){const key={},other=[];const w=new WeakMap([[key,7]]),s=new WeakSet([key,other]);let failures=0;for(const k of [null,undefined,1,'x',false]){try{w.set(k,3);}catch(e){if(e instanceof TypeError)failures++;}try{s.add(k);}catch(e){if(e instanceof TypeError)failures++;}}return [w.set(other,8)===w,s.add(key)===s,w.get(key),w.get(2)===undefined,w.has(null),s.has(other),w.delete(key),w.delete(key),s.delete(other),failures,typeof w.size,typeof s.keys,typeof w.clear,JSON.stringify([w,s])];}")),
        json!([true,true,7,true,false,true,true,false,true,10,"undefined","undefined","undefined","[{},{}]"]));
}

#[test]
fn vm_stage1_iterators_are_live_and_stay_done_after_exhaustion() {
    assert_eq!(returned(runtime("function main(){const m=new Map([['a',1],['b',2]]),it=m.entries();const first=it.next();m.delete('b');m.set('c',3);const second=it.next();m.clear();m.set('d',4);const third=it.next(),done=it.next();m.set('e',5);return [first,second,third,done,it.next(),JSON.stringify(it)];}")),
        json!([{"value":["a",1],"done":false},{"value":["c",3],"done":false},{"value":["d",4],"done":false},{"done":true},{"done":true},"{}"]));
}

#[test]
fn vm_stage1_for_of_and_for_each_observe_mutation() {
    assert_eq!(returned(runtime("function main(){const s=new Set([1,2,3]),seen=[];for(const v of s){seen.push(v);if(v===1){s.delete(2);s.add(4);}}const m=new Map([['a',1],['b',2]]),calls=[];const result=m.forEach((v,k,self)=>{calls.push([v,k,self===m]);if(k==='a'){m.delete('b');m.set('c',3);}});return [seen,calls,result===undefined];}")),
        json!([[1,3,4],[[1,"a",true],[3,"c",true]],true]));
}

#[test]
fn vm_stage1_array_from_mapper_is_interleaved_with_collection_iteration() {
    assert_eq!(returned(runtime("function main(){const s=new Set([1,2]);const a=Array.from(s,v=>{if(v===1)s.add(3);return v*2;});const m=new Map([[1,2]]);const [[key,value]]=m;return [a,key,value,Array.from(m.values()),[...m.entries()]];}")),json!([[2,4,6],1,2,[2],[[1,2]]]));
}

#[test]
fn vm_stage1_collections_have_ordinary_properties_and_json_ignores_entries() {
    assert_eq!(returned(runtime("function main(){const m=new Map();m.set(m,m);m.note=7;const s=Object.freeze(new Set([1]));s.add(2);let failed=false;try{m.size=5;}catch(e){failed=e instanceof TypeError;}return [JSON.stringify(m),Object.keys(m),Object.hasOwn(m,'size'),Object.hasOwn(m,'note'),[...s],failed,m instanceof Map,s instanceof Set,typeof Map];}")),
        json!(["{\"note\":7}",["note"],false,true,[1,2],true,true,true,"function"]));
}

#[test]
fn vm_stage1_constructor_validation_and_unbound_methods_fail_as_type_errors() {
    for expression in ["Map()","Set()","WeakMap()","WeakSet()","new Map([1])","new Map('ab')","new WeakMap([[1,2]])","new WeakSet([1])","new Set(7)","new Map().forEach(3)"] {
        assert_eq!(returned(runtime(&format!("function main(){{try{{{expression};}}catch(e){{return e instanceof TypeError;}}return false;}}"))),json!(true),"{expression}");
    }
    assert_eq!(returned(runtime("function main(){const m=new Map();const get=m.get;try{get(1);}catch(e){return e instanceof TypeError;}return false;}")),json!(true));
}

#[test]
fn vm_stage1_iterator_and_object_identity_survive_host_suspend_and_gc() {
    let mut run=runtime("function main(){let key={id:7};const map=new Map([[key,{n:9}]]),weak=new WeakMap([[key,11]]);const it=map.entries();const set=new Set([1,2,3]),seen=[];key=null;for(let i=0;i<400;i++){const garbage={i};}io.read({});const entry=it.next().value;map.set(entry[0],entry[1]);for(const value of set){seen.push(value);io.read({});}return [entry[0].id,entry[1].n,weak.get(entry[0]),map.size,it.next().done,seen];}");
    for _ in 0..4 {
        assert!(matches!(run.next(),State::Action(_)));
        run=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(json!({}),false).unwrap();
    }
    assert_eq!(returned(run),json!([7,9,11,1,true,[1,2,3]]));
}

#[test]
fn vm_stage1_for_each_callback_resumes_in_the_same_iteration() {
    let mut run=runtime("function main(){const map=new Map([[1,'a'],[2,'b']]),seen=[];map.forEach((v,k,self)=>{seen.push(v);io.read({});if(k===1)self.set(3,'c');});return seen;}");
    for _ in 0..3 {
        assert!(matches!(run.next(),State::Action(_)));
        run=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(json!({}),false).unwrap();
    }
    assert_eq!(returned(run),json!(["a","b","c"]));
}

fn budgeted_runtime(source: &str, limit: usize) -> Runtime {
    let mut snapshot=serde_json::to_value(runtime(source)).unwrap();
    snapshot["fuel"]=json!(limit);
    snapshot["segment_limit"]=json!(limit);
    serde_json::from_value(snapshot).unwrap()
}

#[test]
fn vm_stage0_budget_reports_counts_and_hottest_source_line() {
    let mut run=budgeted_runtime("function main(){\nlet n=0;\nwhile(true){n++;}\n}",64);
    let State::Failed(e)=run.next() else {panic!("unbounded loop must stop")};
    assert_eq!(e["code"],"INSTRUCTION_BUDGET_EXHAUSTED");
    assert_eq!(e["limit"],64);
    assert_eq!(e["used"],64);
    assert_eq!(e["hot_lines"][0]["line"],3);
    assert!(e["hot_lines"][0]["instructions"].as_u64().unwrap()>0);
}

#[test]
fn vm_stage0_host_resume_refreshes_segment_but_snapshot_does_not() {
    let mut run=budgeted_runtime("function main(){let n=0;for(let i=0;i<12;i++){n++;}io.read({});for(let i=0;i<12;i++){n++;}return n;}",220);
    assert!(matches!(run.next(),State::Action(_)));
    let before=serde_json::to_value(&run).unwrap();
    let mut restored:Runtime=serde_json::from_value(before.clone()).unwrap();
    assert!(matches!(restored.next(),State::Action(_)));
    assert_eq!(serde_json::to_value(&restored).unwrap()["fuel"],before["fuel"]);
    restored.resume(json!({}),false).unwrap();
    assert_eq!(serde_json::to_value(&restored).unwrap()["fuel"],220);
    assert_eq!(returned(restored),24);
}

#[test]
fn vm_stage0_saved_budget_and_whole_program_deadline_survive_restore() {
    let mut run=budgeted_runtime("function main(){io.read({});return 7;}",100);
    assert!(matches!(run.next(),State::Action(_)));
    let mut snapshot=serde_json::to_value(run).unwrap();
    snapshot["started_at_ms"]=json!(chrono::Utc::now().timestamp_millis()-10000);
    snapshot["time_limit_ms"]=json!(1000);
    let mut restored:Runtime=serde_json::from_value(snapshot).unwrap();
    restored.resume(json!({}),false).unwrap();
    let State::Failed(e)=restored.next() else {panic!("host interaction must not restart the whole-program deadline")};
    assert_eq!(e["code"],"PROGRAM_TIME_BUDGET_EXHAUSTED");
    assert_eq!(e["limit_ms"],1000);
    assert!(e["elapsed_ms"].as_i64().unwrap()>=10000);
}

#[test]
fn vm_stage0_unsupported_syntax_diagnostics_name_the_feature() {
    for (source,name) in [("new WeakRef({})","WeakRef"),("new Proxy({}, {})","Proxy"),("new URL('https://example.com')","URL"),("class Box { #x; }","class")] {
        let error=compile(source).unwrap_err().to_string();
        assert!(error.contains(name),"{source}: {error}");
    }
}

#[test]
fn output_capacity_complete_console_records_survive_snapshot() {
    let mut run=runtime("function main(){console.log('BEGIN'+'x'.repeat(110000)+'END');for(let i=0;i<250;i++){console.log('record '+i);}io.read({});return 'done';}");
    assert!(matches!(run.next(),State::Action(_)));
    let mut restored:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert_eq!(restored.logs().len(),251);
    assert_eq!(restored.logs()[0].len(),110008);
    assert!(restored.logs()[0].ends_with("END"));
    assert_eq!(restored.logs()[250],"record 249");
    restored.resume(json!({}),false).unwrap();
    assert_eq!(returned(restored),json!("done"));
}

#[test]
fn output_capacity_capture_exhaustion_is_fatal_and_retains_rejected_record() {
    let mut run=runtime("function main(){try{for(let i=0;i<20;i++){console.log('BEGIN'+'x'.repeat(1000000)+'END');}}catch(e){io.write({path:'bad',content:'partial success'});}return 'success';}");
    let State::Failed(error)=run.next() else {panic!("capture exhaustion must stop before catch effects")};
    assert_eq!(error["code"],"OUTPUT_OVER_CAPACITY");
    assert_eq!(error["delivered"],false);
    assert_eq!(run.logs().len(),16);
    let mut restored:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert!(restored.rejected_console().unwrap().ends_with("END"));
    assert_eq!(restored.rejected_console().unwrap().len(),1_000_008);
    assert!(matches!(restored.next(),State::Failed(_)));
}

#[test]
fn output_capacity_host_failure_cannot_be_caught_as_partial_success() {
    for parallel in [false,true] {
        let mut run=runtime(if parallel {
            "async function main(){try{await io.read({});}catch(e){io.write({path:'bad'});}return 'success';}"
        } else {
            "function main(){try{io.read({});}catch(e){io.write({path:'bad'});}return 'success';}"
        });
        let State::Action(action)=run.next() else {panic!("expected read")};
        let error=json!({"code":"SHELL_OUTPUT_OVER_CAPACITY","delivered":false,"output_asset":{"asset_id":"saved"}});
        if parallel {run.resume_action(&action.id,error,true).unwrap();}
        else {run.resume(error,true).unwrap();}
        let State::Failed(error)=run.next() else {panic!("capacity cannot become success")};
        assert_eq!(error["output_asset"]["asset_id"],"saved");
    }
}

#[test]
fn compatibility_array_constructor_length_holes_fill_and_assignment() {
    assert_eq!(returned(runtime(r#"async function main(){
      const sparse=new Array(3); sparse[1]=7;
      return [new Array(3).fill(0), Array(3).length, new Array(1,2),
        Array('3'), Array(), JSON.stringify(sparse), 0 in sparse, 1 in sparse,
        Object.keys(new Array(2)), Array(2).map(x=>1)];
    }"#)), json!([[0,0,0],3,[1,2],["3"],[],"[null,7,null]",false,true,[],[null,null]]));
    for expression in ["Array(-1)", "new Array(1.5)", "Array(NaN)", "Array(Infinity)"] {
        assert_eq!(returned(runtime(&format!("function main(){{try{{{expression};}}catch(e){{return e instanceof RangeError;}}return false;}}"))), json!(true));
    }
}

#[test]
fn compatibility_raw_templates_and_character_constructors() {
    assert_eq!(returned(runtime(r#"function main(){
      const raw=String.raw`a\nb`;
      return [raw, raw.length, String.raw`x${1}\d`, String.raw`\uZZZZ`,
        String.fromCharCode('A'.charCodeAt(0)+1), String.fromCharCode(65537,NaN,-1),
        String.fromCharCode(0xd83d,0xde00),String.fromCodePoint(0x1f600)];
    }"#)), json!(["a\\nb",4,"x1\\d","\\uZZZZ","B","\u{1}\0\u{ffff}","😀","😀"]));
    for expression in ["String.fromCodePoint(-1)","String.fromCodePoint(0x110000)","String.fromCodePoint(1.5)"] {
        assert_eq!(returned(runtime(&format!("function main(){{try{{{expression};}}catch(e){{return e instanceof RangeError;}}}}"))), json!(true));
    }
}

#[test]
fn compatibility_regexp_named_groups_and_replacement() {
    assert_eq!(returned(runtime(r#"function main(){
      const r=/(?<y>\d+)(?<optional>x)?/;
      const e=r.exec('a12'); const m='a34'.match(r);
      const all='a1b2'.matchAll(/(?<n>\d)/g);
      return [e.groups.y, e.groups.optional===undefined, e.index,e.input,m.groups.y,
        all.map(x=>x.groups.n),'a12'.replace(r,'$<y>-$<optional>'),
        /x/.exec('x').groups===undefined,'a12'.replace(r,(whole,y,opt,index,input,groups)=>groups.y)];
    }"#)),json!(["12",true,1,"a12","34",["1","2"],"a12-",true,"a12"]));
    // Named groups are heap references and must remain live across a host call.
    let mut run=runtime(r"function main(){const m=/(?<y>\d+)/.exec('a12'); io.read({}); return m.groups.y;}");
    assert!(matches!(run.next(),State::Action(_)));
    let mut restored:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    restored.resume(json!({}),false).unwrap();
    assert_eq!(returned(restored),json!("12"));
}

#[test]
fn compatibility_has_own_freeze_and_deep_clone() {
    assert_eq!(returned(runtime(r#"function main(){
      const o={x:1,nested:{y:2}}; const frozen=Object.freeze(o); const copy=structuredClone(o);
      copy.nested.y=3; let failures=0;
      try{o.x=2;}catch(e){if(e instanceof TypeError) failures++;}
      try{o.newKey=2;}catch(e){if(e instanceof TypeError) failures++;}
      try{delete o.x;}catch(e){if(e instanceof TypeError) failures++;}
      try{Object.assign(o,{x:4});}catch(e){if(e instanceof TypeError) failures++;}
      const a=Object.freeze([1,2]);
      try{a.push(3);}catch(e){if(e instanceof TypeError) failures++;}
      try{a.fill(0);}catch(e){if(e instanceof TypeError) failures++;}
      try{a[0]=7;}catch(e){if(e instanceof TypeError) failures++;}
      return [frozen===o,o.x,o.nested.y,copy!==o,copy.nested!==o.nested,copy.nested.y,
        failures,Object.hasOwn(o,'x'),Object.hasOwn(o,'toString'),Object.hasOwn(Array(2),'0'),Object.hasOwn(a,'length')];
    }"#)),json!([true,1,2,true,true,3,7,true,false,false,true]));
}

#[test]
fn compatibility_freeze_survives_host_suspend() {
    let mut run=runtime("function main(){const o=Object.freeze({x:1});io.read({});try{o.x=2;}catch(e){return [e instanceof TypeError,o.x];}}");
    assert!(matches!(run.next(),State::Action(_)));
    let mut restored:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    restored.resume(json!({}),false).unwrap();
    assert_eq!(returned(restored),json!([true,1]));
}

#[test]
fn compatibility_uri_encoding_and_malformed_inputs() {
    assert_eq!(returned(runtime(r#"function main(){return [encodeURIComponent('中文 😀/a?b+c'),
      decodeURIComponent('%E4%B8%AD%E6%96%87%20%F0%9F%98%80%2Fa%3Fb%2Bc'),
      encodeURIComponent("-_.!~*'()"),decodeURIComponent('a+b')];}"#)),
      json!(["%E4%B8%AD%E6%96%87%20%F0%9F%98%80%2Fa%3Fb%2Bc","中文 😀/a?b+c","-_.!~*'()","a+b"]));
    for text in ["%","%GG","%C0%AF","%ED%A0%80","%F4%90%80%80","%E4%B8"] {
        assert_eq!(returned(runtime(&format!("function main(){{try{{decodeURIComponent('{}');}}catch(e){{return e instanceof URIError;}}}}",text))), json!(true));
    }
    assert_eq!(returned(runtime(r#"function main(){let failures=0;try{encodeURIComponent('\ud800');}catch(e){if(e instanceof URIError)failures++;}try{encodeURIComponent(String.fromCharCode(0xdc00));}catch(e){if(e instanceof URIError)failures++;}return [failures,JSON.stringify(String.fromCodePoint(0xd800))];}"#)),json!([2,"\"\\ud800\""]));
    assert_eq!(returned(runtime(r#"function main(){let bad=false;try{encodeURIComponent(`x${String('\ud800')}`);}catch(e){bad=e instanceof URIError;}return [bad,JSON.stringify(decodeURIComponent('\ud800%41')),encodeURIComponent('\ud83d'+'\ude00')];}"#)),json!([true,"\"\\ud800A\"","%F0%9F%98%80"]));
}

#[test]
fn compatibility_diagnostics_name_missing_global_and_host_import_syntax() {
    let error=compile("function main(){return absentGlobal(3);}").unwrap_err().to_string();
    assert!(error.contains("absentGlobal 在流程运行时里不可用"),"{error}");
    assert!(!error.contains("call/ai wrappers"),"{error}");
    let error=compile("function main(){print('result');}").unwrap_err().to_string();
    assert!(error.contains("print 在流程运行时里不可用") && error.contains("console.log(...)"),"{error}");
    let error=compile("function main(){const a=require('host:attachments');}").unwrap_err().to_string();
    assert!(error.contains("import { read_attachment } from \"host:attachments\"") && error.contains("不要用 require"),"{error}");
}

#[test]
fn compatibility_date_constructors_utc_arithmetic_and_json() {
    assert_eq!(returned(runtime(r#"async function main(){
      const epoch=new Date(0);const millennium=new Date('2000-01-01T00:00:00Z');
      return [typeof Date.now(),typeof new Date().getTime(),epoch.toISOString(),millennium.getTime(),Date.UTC(2000,0,1),
        Date.parse('2000-01-01T00:00:00Z'),epoch.getUTCFullYear(),epoch.getUTCMonth(),epoch.getUTCDate(),epoch.getUTCDay(),
        epoch.getUTCHours(),epoch.getUTCMinutes(),epoch.getUTCSeconds(),epoch.getUTCMilliseconds(),
        new Date(1000)-epoch,epoch<new Date(1),epoch<=new Date(0),new Date(1)>epoch,new Date(1)>=epoch,+epoch,
        JSON.stringify({t:epoch}),epoch.toJSON(),epoch.valueOf(),epoch instanceof Date,new Date(epoch).getTime()];
    }"#)), json!(["number","number","1970-01-01T00:00:00.000Z",946684800000i64,946684800000i64,946684800000i64,
        1970,0,1,4,0,0,0,0,1000,true,true,true,true,0,"{\"t\":\"1970-01-01T00:00:00.000Z\"}","1970-01-01T00:00:00.000Z",0,true,0]));
}

#[test]
fn compatibility_date_invalid_and_local_components() {
    assert_eq!(returned(runtime(r#"function main(){
      const invalid=new Date('x');let range=false;try{invalid.toISOString();}catch(e){range=e instanceof RangeError;}
      const local=new Date(2000,0,2,3,4,5,6);
      return [Number.isNaN(invalid.getTime()),range,invalid.toString(),invalid.toJSON(),JSON.stringify(invalid),
        local.getFullYear(),local.getMonth(),local.getDate(),local.getHours(),local.getMinutes(),local.getSeconds(),local.getMilliseconds(),
        local.getDay(),new Date(99,0,1).getFullYear(),Number.isNaN(new Date(8640000000000001).getTime())];
    }"#)),json!([true,true,"Invalid Date",null,"null",2000,0,2,3,4,5,6,0,1999,true]));
    for expression in ["new Date().toLocaleString()","new Date().toLocaleDateString()","new Date().toLocaleTimeString()"] {
        let error=compile(&format!("function main(){{io.write({{}});return {expression};}}")).unwrap_err().to_string();
        assert!(error.contains("locale data") && error.contains("toISOString"),"{error}");
    }
}

#[test]
fn compatibility_date_survives_host_suspend_and_snapshot() {
    let mut run=runtime(r#"async function main(){const d=new Date(0);const invalid=new Date('x');io.read({});return [d.toISOString(),d instanceof Date,+d,Number.isNaN(invalid.getTime())];}"#);
    assert!(matches!(run.next(),State::Action(_)));
    let mut restored:Runtime=serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    restored.resume(json!({}),false).unwrap();
    assert_eq!(returned(restored),json!(["1970-01-01T00:00:00.000Z",true,0,true]));
}

#[test]
fn compatibility_builtin_contracts_parse_and_match_available_apis() {
    use swc_common::{FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
    let map:Lrc<SourceMap>=Default::default();
    let file=map.new_source_file(FileName::Custom("builtin-contracts.d.ts".into()).into(), BUILTIN_CONTRACTS.to_owned());
    let lexer=Lexer::new(Syntax::Typescript(TsSyntax{dts:true,..Default::default()}),Default::default(),StringInput::from(&*file),None);
    let mut parser=Parser::new_from(lexer);
    parser.parse_typescript_module().expect("published builtin contracts parse as declarations");
    assert!(parser.take_errors().is_empty());
    for name in ["Array","String.fromCharCode","String.fromCodePoint","Object.hasOwn","Object.freeze","encodeURIComponent","decodeURIComponent","structuredClone","Date.now","Date.parse","Date.UTC"] {
        assert!(js_methods::builtin_arity(name).is_some(),"{name}");
        assert!(CAPABILITIES.contains(name),"guide omits {name}");
    }
}

// Test-only effect targets, declared just like plugin methods.
fn compile(source: &str) -> anyhow::Result<Program> {
    let mut bindings = libraries::builtin();
    bindings.insert(
        "host".into(),
        ["inspect_agent_changes", "merge_agent_result"]
            .into_iter()
            .map(|n| (n.into(), n.into()))
            .collect(),
    );
    bindings.insert(
        "test".into(),
        ["record", "never", "selected"]
            .into_iter()
            .map(|n| (n.into(), n.into()))
            .collect(),
    );
    compile_with_libraries(source, bindings)
}
/// The function a test drives: its `main` function when it declares one and
/// has no top-level statements, otherwise the top-level program.
fn entry_of(program: &Program) -> &'static str {
    let top_level = program.functions[ENTRY].code.len() > 1;
    if !top_level && program.functions.contains_key("main") { "main" } else { ENTRY }
}
fn start(source: &str, args: Vec<Value>) -> Runtime {
    let program = compile(source).unwrap();
    let entry = entry_of(&program);
    Runtime::new(program, entry, args).unwrap()
}
fn start_task(program: Program, args: Vec<Value>) -> Runtime {
    let entry = entry_of(&program);
    Runtime::task_program(program, entry, args).unwrap()
}
fn runtime(source: &str) -> Runtime {
    start(source, vec![])
}
fn returned(mut r: Runtime) -> Value {
    match r.next() {
        State::Returned(v) => v,
        other => panic!("expected return, got {other:?}"),
    }
}

#[test]
fn regular_expressions_validate_limits_and_resume_with_saved_state() {
    for source in [
        r"function main(){return /(?=x)/.test('x');}",
        r"function main(){return /(a)\1/.test('aa');}",
        r"function main(){return /x/y.test('x');}",
        r"function main(){return /x/gg.test('x');}",
    ] {
        assert!(compile(source).is_err(), "should reject {source}");
    }
    assert_eq!(
        returned(runtime(r"function main(){return 'b'.match(/(a)?b/);}")),
        json!(["b", null])
    );
    assert_eq!(
        returned(runtime(
            r"function main(){const r=/a/g;return [r.test('a'),r.test('a')];}"
        )),
        json!([true, true])
    );
    let mut heap = heap::Heap::default();
    let plain = heap
        .from_json(&json!({"$flow_regex":{"pattern":"a","flags":""}}))
        .unwrap();
    let global = heap
        .from_json(&json!({"$flow_regex":{"pattern":"a","flags":"g"}}))
        .unwrap();
    let input = heap::VmValue::String("a".repeat(2 * 1024 * 1024 + 1));
    assert!(js_methods::call_method(&mut heap, plain, "test", &[input]).is_err());
    let many = heap::VmValue::String("a".repeat(10001));
    assert!(js_methods::call_method(&mut heap, many, "match", &[global]).is_err());
    let mut run = runtime(
        r"function main(){const pattern=/(\d+)/;const data=io.read({path:'a'});return data.content.match(pattern);}",
    );
    assert!(matches!(run.next(), State::Action(_)));
    let mut restored: Runtime =
        serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    restored.resume(json!({"content":"page42"}), false).unwrap();
    assert_eq!(returned(restored), json!(["42", "42"]));
}

#[test]
fn text_checks_can_pass_real_read_content_to_ai_review() {
    let mut run = runtime(
        r#"
function main() {
  const file = io.read({path:"page.html"});
  const links = file.content.match(/href="[^"]+"/g);
  const summary = {hasTitle: /<title>[^<]+<\/title>/i.test(file.content), links: links};
  return inspect(file.content, summary);
}

/** @virtual */
function inspect(content, summary) { // Judge the page using the actual content and deterministic checks.
}
"#,
    );
    assert!(
        matches!(run.next(), State::Action(action) if matches!(action.kind, ActionKind::Tool { ref name, .. } if name == "read_file"))
    );
    run.resume(
        json!({"content":"<title>Test</title><a href=\"one.html\">One</a>"}),
        false,
    )
    .unwrap();
    let State::Action(action) = run.next() else {
        panic!("missing AI review")
    };
    assert!(
        matches!(action.kind, ActionKind::Call { ref name, ref args, .. } if name == "inspect" && args[1] == json!({"hasTitle":true,"links":["href=\"one.html\""]})),
        "unexpected action: {:?}",
        action.kind
    );
    run.resume(json!({"passed":true}), false).unwrap();
    assert_eq!(returned(run), json!({"passed":true}));
}

#[test]
fn rust_path_and_url_builtins_run_inside_flow_and_survive_resume() {
    let mut run = runtime(
        r#"
function main() {
  const target = path.join("docs", "pages", "..", "index.html");
  const link = url.parse("../asset.css?x=1#part", "https://example.com/docs/pages/");
  const input = io.read({path:"proof.txt"});
  return {target:target, link:link, data:input.content};
}
"#,
    );
    assert!(
        matches!(run.next(), State::Action(action) if matches!(action.kind, ActionKind::Tool { ref name, .. } if name == "read_file"))
    );
    let mut restored: Runtime =
        serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    restored.resume(json!({"content":"ok"}), false).unwrap();
    let result = returned(restored);
    assert_eq!(
        result["target"],
        std::path::Path::new("docs")
            .join("index.html")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(
        result["link"]["href"],
        "https://example.com/docs/asset.css?x=1#part"
    );
    assert_eq!(result["data"], "ok");
    assert_eq!(
        returned(runtime(
            "function main(){return path.basename('a/b.txt','.txt');}"
        )),
        json!("b")
    );
    assert_eq!(
        returned(runtime(
            "function main(){return path.join('a', '$(echo nope)');}"
        )),
        json!(
            std::path::Path::new("a")
                .join("$(echo nope)")
                .to_string_lossy()
                .to_string()
        )
    );
    assert!(compile("function main(){return path.nonexistent('x');}").is_err());
    assert_eq!(
        returned(runtime(
            "function main(){try{return path.resolve('relative');}catch(error){return error.message;}}"
        )),
        json!("path.resolve requires an absolute path argument")
    );
    let invalid = returned(runtime(
        "function main(){try{return url.parse('not a URL');}catch(error){return error.message;}}",
    ));
    assert!(invalid.as_str().unwrap().starts_with("URL parse error:"));
}


#[test]
fn await_promise_all_suspends_once_and_returns_ordered_read_results() {
    let mut run = runtime(
        r#"
import * as io from "host:io/promises";
async function main() {
  const paths = ["first.txt", "second.txt"];
  return await Promise.all([
    io.read({path:paths[0]}),
    io.read({path:paths[1]}),
    io.search({path:".",query:"needle"})
  ]);
}"#,
    );
    let State::Action(action) = run.next() else {
        panic!("missing parallel reads")
    };
    match action.kind {
        ActionKind::ParallelReads { operations } => {
            assert_eq!(operations.len(), 3);
            assert_eq!(operations[0].0, "read_file");
            assert_eq!(operations[0].1["path"], "first.txt");
            assert_eq!(operations[1].1["path"], "second.txt");
            assert_eq!(operations[2].0, "search_text");
        }
        other => panic!("unexpected action {other:?}"),
    }
    let mut recovered: Runtime =
        serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    let State::Action(again) = recovered.next() else {
        panic!("missing saved action")
    };
    assert_eq!(again.id, action.id);
    recovered
        .resume(
            json!([{"failed":false,"value":{"content":"first"}},{"failed":false,"value":{"content":"second"}},{"failed":false,"value":{"matches":[]}}]),
            false,
        )
        .unwrap();
    assert_eq!(
        returned(recovered),
        json!([{"content":"first"},{"content":"second"},{"matches":[]}])
    );
}

#[test]
fn promise_all_accepts_synchronous_effects_with_javascript_ordering() {
    let mut run = runtime("async function main(){return await Promise.all([io.write({path:'a',content:'b'})]);}");
    assert!(matches!(run.next(), State::Action(a) if matches!(a.kind, ActionKind::Tool { ref name, .. } if name == "write_file")));
    run.resume(json!({"ok":true}), false).unwrap();
    assert_eq!(returned(run), json!([{"ok":true}]));
}

#[test]
fn failed_parallel_reads_follow_catch_without_repeating_the_batch() {
    let mut run = runtime(
        r#"
import * as io from "host:io/promises";
async function main() {
  try { return await Promise.all([io.read({path:"a"}),io.read({path:"b"})]); }
  catch(error) { return error.message; }
}"#,
    );
    assert!(
        matches!(run.next(), State::Action(action) if matches!(action.kind, ActionKind::ParallelReads { .. }))
    );
    run.resume(json!({"message":"read failed"}), true).unwrap();
    assert_eq!(returned(run), json!("read failed"));
}


#[test]
fn virtual_function_body_can_be_refined_to_an_async_entry() {
    let program = compile_body(
        r#"
import * as io from "host:io/promises";
async function review(paths) {
  return await Promise.all(paths.map(path => io.read({path:path})));
}"#,
        "review",
    )
    .unwrap();
    let mut run = Runtime::task_program(program, "review", vec![json!(["a", "b"])]).unwrap();
    let State::Action(action) = run.next() else {
        panic!("missing read batch")
    };
    assert!(
        matches!(action.kind, ActionKind::ParallelReads { ref operations } if operations.len() == 2)
    );
    run.resume(json!([{"failed":false,"value":{"content":"a"}},{"failed":false,"value":{"content":"b"}}]), false)
        .unwrap();
    assert_eq!(returned(run), json!([{ "content":"a" },{ "content":"b" }]));
    assert!(compile_body("async function review(){return review();}", "review").is_err());
}
#[test]
fn data_loops_local_calls_and_destructuring_need_no_operators() {
    let mut r = runtime(
        r#"
function echo(input) {return input;}
function main() {
  const result = echo({ready:true, rows:[{n:2},{n:3}]});
  for (const item of result.rows) {test.record({item});}
  const [a,b] = result.rows;
  if(result.ready){return {a,b};} else {throw 'not ready';}
}"#,
    );
    for n in [2, 3] {
        let State::Action(action) = r.next() else {
            panic!()
        };
        assert!(
            matches!(action.kind,ActionKind::Tool{ref name,ref args} if name=="record" && args["item"]["n"]==n)
        );
        r.resume(json!({}), false).unwrap();
    }
    assert_eq!(returned(r), json!({"a":{"n":2},"b":{"n":3}}));
}
#[test]
fn suspended_effect_serializes_and_resumes_once() {
    let mut r = runtime(
        "function main(){ const receipt = io.write({path:'a.txt',content:'x'}); return receipt.version; }",
    );
    let State::Action(first) = r.next() else {
        panic!()
    };
    assert!(matches!(first.kind,ActionKind::Tool{ref name,..} if name=="write_file"));
    let mut restored: Runtime = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
    let State::Action(again) = restored.next() else {
        panic!()
    };
    assert_eq!(first.id, again.id);
    restored.resume(json!({"version":"v1"}), false).unwrap();
    assert!(restored.resume(json!({}), false).is_err());
    assert_eq!(returned(restored), json!("v1"));
}
#[test]
fn child_failure_unwinds_to_callers_catch_and_skips_following_effect() {
    let mut r = runtime(
        "function child(){ io.read({path:'missing'}); io.write({path:'bad'}); } function main(){ try { child(); } catch(error) { return error.message; } }",
    );
    assert!(matches!(r.next(), State::Action(_)));
    r.resume(json!({"message":"not found"}), true).unwrap();
    assert_eq!(returned(r), json!("not found"));
    let mut r = runtime("function main(){ io.read({path:'missing'}); io.write({path:'bad'}); }");
    assert!(matches!(r.next(), State::Action(_)));
    r.resume(json!({"message":"not found"}), true).unwrap();
    let State::Failed(error) = r.next() else {
        panic!()
    };
    assert_eq!(error["message"], "not found");
    assert!(error["location"]["line"].is_number());
}
#[test]
fn unsupported_code_is_rejected_with_location_before_effects() {
    for source in [
        "function main(){io.write({path:'bad'});\n fetch('https://example.com');}",
        "function main(){const x=1; x=2;}",
        "import fs from 'fs'; function main(){}",
        "function main(){tools.create_task({});}",
    ] {
        let error = compile(source).unwrap_err().to_string();
        assert!(error.starts_with("TS_PROGRAM "), "{error}");
        assert!(error.contains(':'), "missing source location: {error}");
    }
}

#[test]
fn call_depth_errors_are_catchable() {
    let r = runtime(
        "function recurse(){return recurse();} function main(){try {return recurse();} catch(e){return e.message;}}",
    );
    assert_eq!(returned(r), json!("program call depth limit"));
}

#[test]
fn deterministic_expressions_return_without_model_actions() {
    for (expression, expected) in [
        ("1+2*3", json!(7)),
        ("5/2", json!(2.5)),
        ("-5%2", json!(-1)),
        ("1===1", json!(true)),
        ("1 !== '1'", json!(true)),
        ("[1,{x:2}]===[1,{x:2}]", json!(false)),
        ("!false", json!(true)),
        ("typeof 'abc'", json!("string")),
        ("typeof null", json!("object")),
        ("typeof [1]", json!("object")),
        ("Array.isArray([1])", json!(true)),
        ("Array.isArray({})", json!(false)),
        ("Object.keys({b:2,a:1})", json!(["b", "a"])),
        ("Object.values({b:2,a:1})", json!([2, 1])),
        ("+3", json!(3)),
        ("false && true", json!(false)),
        ("true || false", json!(true)),
        ("null ?? 4", json!(4)),
        ("false ?? 4", json!(false)),
        ("true ? 1 : 2", json!(1)),
        ("`value:${1+2}`", json!("value:3")),
        ("'a'+'b'", json!("ab")),
        ("'中文😀'.length", json!(3)),
        ("'中文😀'.indexOf('😀')", json!(2)),
        ("'abcd'.slice(1,-1)", json!("bc")),
        ("[1,2,3].slice(1)", json!([2, 3])),
        ("[1,2,3].at(-1)", json!(3)),
        ("'中文'.at(-1)", json!("文")),
        ("[1].at(2)", Value::Null),
        ("[1].concat([2,3])", json!([1, 2, 3])),
        ("'a'.concat('b')", json!("ab")),
        ("' a '.trim().toUpperCase()", json!("A")),
        ("'a,b'.split(',')", json!(["a", "b"])),
        ("'中a中'.lastIndexOf('中')", json!(2)),
        ("'aaa'.replace('a','x')", json!("xaa")),
        ("'aba'.replaceAll('a','x')", json!("xbx")),
        ("'中a'.replaceAll('','-')", json!("-中-a-")),
        ("/abc/i.test('ABC')", json!(true)),
        (r"'x12'.match(/(\d+)/)", json!(["12", "12"])),
        (r"'a1b2'.match(/\d/g)", json!(["1", "2"])),
        ("'abc'.match(/z/)", Value::Null),
        ("[1,true,null].join('-')", json!("1-true-")),
        ("[1,2].includes(2)", json!(true)),
        ("JSON.parse(JSON.stringify({ok:true}))", json!({"ok":true})),
        ("9007199254740993 + 2", json!(9007199254740994u64)),
    ] {
        for _ in 0..3 {
            assert_eq!(
                returned(runtime(&format!("function main(){{return {expression};}}"))),
                expected,
                "{expression}"
            );
        }
    }
    assert!(compile("async function* main(){yield io.read({path: 'x'});}").is_ok());
    assert_eq!(
        returned(runtime(
            "function main(){return 'function* x(){yield call();}';}"
        )),
        json!("function* x(){yield call();}")
    );
}

#[test]
fn older_runtime_snapshots_without_promise_state_remain_readable() {
    let mut r = runtime("function main(){io.read({path:'input'});return 42;}");
    assert!(matches!(r.next(), State::Action(_)));
    let mut snapshot = serde_json::to_value(&r).unwrap();
    snapshot.as_object_mut().unwrap().remove("promises");
    snapshot.as_object_mut().unwrap().remove("heap");
    let mut restored: Runtime = serde_json::from_value(snapshot).unwrap();
    restored.resume(json!({}), false).unwrap();
    assert_eq!(returned(restored), json!(42));
}

#[test]
fn vm_object_aliases_survive_calls_and_host_suspension() {
    let mut r = runtime("function main(){const a={inner:{x:1}};const b=a;io.read({path:'input'});b.inner.x=2;return a.inner.x;}");
    assert!(matches!(r.next(), State::Action(_)));
    let snapshot = serde_json::to_vec(&r).unwrap();
    let mut restored: Runtime = serde_json::from_slice(&snapshot).unwrap();
    restored.resume(json!({}), false).unwrap();
    assert_eq!(returned(restored), json!(2));
    assert_eq!(returned(runtime("function set(o){o.x=9;} function main(){const a={x:1};set(a);return a.x;}")), json!(9));
    assert_eq!(returned(runtime("function main(){const a=[1];const b=a;b[0]=3;return [a===b,a===[3],a[0]];}")), json!([true,false,3]));
    assert_eq!(returned(runtime("function main(){const a={b:1,a:2};return Object.keys(a);}")), json!(["b","a"]));
    assert_eq!(returned(runtime("function main(){const item={x:1};const rows=[item];const copy=rows.slice(0);copy[0].x=7;return [rows[0].x,rows.includes({x:7}),rows.includes(item)];}")), json!([7,false,true]));
    assert_eq!(returned(runtime("function empty(){return;} function main(){let x;return [typeof x,typeof undefined,typeof empty(),x===undefined];}")), json!(["undefined","undefined","undefined",true]));
    assert_eq!(returned(runtime("function main(){const a={};a.self=a;for(var i=0;i<1000;i++){}return a.self===a;}")), json!(true));
    assert_eq!(returned(runtime("function main(){const a=[2,3];let i=0;const old=a[i++]++;a[0]+=a[1];return [i,old,a[0],a[1]];}")), json!([1,2,6,3]));
    assert_eq!(returned(runtime("function main(){const a=[1,2,3];a.length=1;return a;}")), json!([1]));
}

#[test]
fn dynamic_regexp_constructor_evaluates_arguments_and_survives_suspension() {
    assert_eq!(returned(runtime(r#"function main(){
        const pattern = "a+"; const flags = "i";
        return new RegExp(pattern, flags).test("AAA");
    }"#)), json!(true));
    assert_eq!(returned(runtime(r#"function main(){
        const original = /abc/i; const copy = new RegExp(original);
        return copy.test("ABC");
    }"#)), json!(true));
    assert_eq!(returned(runtime(r#"function main(){ return new RegExp().test(""); }"#)), json!(true));
    let mut r = runtime(r#"function main(){
        const pattern = "a+"; io.read({path:"input"});
        return new RegExp(pattern, "i").test("AAA");
    }"#);
    assert!(matches!(r.next(), State::Action(_)));
    let snapshot = serde_json::to_string(&r).unwrap();
    let mut restored: Runtime = serde_json::from_str(&snapshot).unwrap();
    restored.resume(json!({}), false).unwrap();
    assert_eq!(returned(restored), json!(true));
    let mut invalid = runtime(r#"function main(){ return new RegExp("x", "ii").test("x"); }"#);
    assert!(matches!(invalid.next(), State::Failed(_)));
}

#[test]
fn executable_extensions_and_unknown_data_methods_are_rejected_before_effects() {
    for expression in [
        "x!",
        "(1,2)",
    ] {
        let error = compile(&format!(
            "function main(){{let x=1;io.write({{path:'never'}});\nreturn {expression};}}"
        ))
        .unwrap_err()
        .to_string();
        assert!(error.starts_with("TS_PROGRAM 2:"), "{error}");
    }
    assert!(compile("function main(){const x=1;x=2;}").is_err());
    assert_eq!(returned(runtime("function main(){const x={};x.a=2;return x.a;}")), json!(2));
    // delete and logical assignment are JavaScript, not extensions.
    assert_eq!(
        returned(runtime("function main(){const x={a:1};const removed=delete x.a;let y=1;y&&=true;let z=null;z??=1;return [removed,x,y,z];}")),
        json!([true, {}, true, 1])
    );
    // A call of something that is not a function compiles and throws when it runs.
    assert_eq!(
        returned(runtime("function main(){let x=1;const run=(()=>1)();try{x.unknown();}catch(error){return [run,error.name];}}")),
        json!([1, "TypeError"])
    );
}

#[test]
fn array_map_filter_callbacks_capture_inputs_and_survive_tool_suspension() {
    let r = runtime("function main(){const factor=2;const rows=[2,3,4];const result=rows.map((v,i,a)=>v*factor+i+a.length);return result.filter(function(v){return v>8;});}");
    assert_eq!(returned(r), json!([10,13]));
    assert_eq!(returned(runtime("function main(){let sum=0;const a=[1,2];const b=a.map(v=>v+=1);a.map(v=>sum+=v);return [b,sum,a];}")), json!([[2,3],3,[1,2]]));
    assert_eq!(returned(runtime("function main(){const a=[1,2];const b=a.map((v,i,arr)=>arr.push(99));return [a,b];}")), json!([[1,2,99,99],[3,4]]));
    assert_eq!(returned(runtime("function main(){const a=[];a[2]=3;let calls=0;const mapped=a.map(v=>calls+=v);return [mapped,a.filter(v=>true),a.slice(0),a.concat([4]),calls,a.indexOf(undefined),a.includes(undefined)];}")), json!([[null,null,3],[3],[null,null,3],[null,null,3,4],3,-1,true]));
    assert_eq!(returned(runtime("function main(){const a=[1,2];return [a.slice(),'abc'.slice()];}")), json!([[1,2],"abc"]));
    let mut r = runtime("function main(){const paths=['a','b'];return paths.map(path=>io.read({path:path}));}");
    let State::Action(first) = r.next() else { panic!("expected first read") };
    assert!(matches!(first.kind, ActionKind::Tool { ref args, .. } if args["path"] == "a"));
    let mut restored: Runtime = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
    restored.resume(json!({"content":"A"}), false).unwrap();
    let State::Action(second) = restored.next() else { panic!("expected second read") };
    assert!(matches!(second.kind, ActionKind::Tool { ref args, .. } if args["path"] == "b"));
    let mut restored: Runtime = serde_json::from_slice(&serde_json::to_vec(&restored).unwrap()).unwrap();
    restored.resume(json!({"content":"B"}), false).unwrap();
    assert_eq!(returned(restored), json!([{"content":"A"},{"content":"B"}]));
    let mut sparse = runtime("function main(){const a=[];a[1]='b';a[3]='d';return a.map(path=>io.read({path:path}));}");
    let State::Action(first) = sparse.next() else { panic!("expected sparse first read") };
    assert!(matches!(first.kind, ActionKind::Tool { ref args, .. } if args["path"] == "b"));
    sparse.resume(json!({"content":"B"}), false).unwrap();
    let State::Action(second) = sparse.next() else { panic!("expected sparse second read") };
    assert!(matches!(second.kind, ActionKind::Tool { ref args, .. } if args["path"] == "d"));
    let mut sparse: Runtime = serde_json::from_slice(&serde_json::to_vec(&sparse).unwrap()).unwrap();
    sparse.resume(json!({"content":"D"}), false).unwrap();
    assert_eq!(returned(sparse), json!([null,{"content":"B"},null,{"content":"D"}]));
    assert!(matches!(runtime("function main(){return (1).map(v=>v);}").next(), State::Failed(_)));
    assert!(compile("async function main(){return await Promise.all([1].map(async v=>v));}").is_ok());
}

#[test]
fn business_judgment_is_an_explicit_task_with_named_inputs_and_evidence() {
    let source = "function main(要求){let 产物={path:'artifact.html'};const check=检查(产物,要求);if(!check.passed){产物={path:'repaired.html'};}return 产物;} function 检查(产物,要求){/* @virtual */ // Inspect real artifacts; return {passed:boolean,evidence:[...]}.\n}";
    for (passed, path) in [(true, "artifact.html"), (false, "repaired.html")] {
        let mut r = Runtime::new(
            compile(source).unwrap(),
            "main",
            vec![json!({"expected":"approved"})],
        )
        .unwrap();
        let State::Action(action) = r.next() else {
            panic!()
        };
        assert!(
            matches!(action.kind,ActionKind::Call{name,args,..} if name=="检查" && args[0]["path"]=="artifact.html" && args[1]["expected"]=="approved")
        );
        r.resume(
            json!({"passed":passed,"evidence":[{"source":"actual tool receipt"}]}),
            false,
        )
        .unwrap();
        assert_eq!(returned(r), json!({"path":path}));
    }
    assert!(compile("function main(){if(实际产物符合要求){return 1;}return 0;}").is_err());
}

#[test]
fn type_errors_and_assertions_use_the_existing_exception_path() {
    for statement in [
        "JSON.parse('bad');",
    ] {
        assert_eq!(
            returned(runtime(&format!(
                "function main(){{try{{{statement}}}catch(error){{return 'caught';}}return 'missed';}}"
            ))),
            json!("caught"),
            "{statement}"
        );
    }
    for expression in [
        "JSON.parse('bad')",
        "null.x",
        "[].reduce((a,b)=>a)",
    ] {
        let r = runtime(&format!(
            "function main(){{try{{return {expression};}}catch(error){{return {{caught:true,message:error.message}};}}}}"
        ));
        assert_eq!(returned(r)["caught"], true, "{expression}");
    }
    // These convert as in JavaScript instead of throwing.
    assert_eq!(
        returned(runtime("function main(){return [`${{x:1}}`,'x'.slice(0.5)];}")),
        json!(["[object Object]", "x"])
    );
    assert!(
        matches!(runtime("function main(){assert(false,'failed');io.write({path:'never'});}").next(),State::Failed(e) if e["message"]=="failed")
    );
    assert_eq!(
        returned(runtime("function main(){assert(1+1===2);return 'passed';}")),
        json!("passed")
    );
}

#[test]
fn if_else_uses_returned_boolean_or_field_directly() {
    let source = "function check(input){return input;} function main(input){const result=check(input);if(result.ok){return 'yes';}else if(result.retry){return 'retry';}else{return 'no';}}";
    for (input, expected) in [
        (json!({"ok":true}), "yes"),
        (json!({"ok":false,"retry":true}), "retry"),
        (json!({"ok":false,"retry":false}), "no"),
    ] {
        assert_eq!(
            returned(start(source, vec![input])),
            json!(expected)
        );
    }
    let mut r = runtime(
        "function main(){const ok=check();if(ok){return 'yes';}else{return 'no';}} function check(){/* @virtual */ // Return a boolean conclusion from actual evidence.\n}",
    );
    assert!(matches!(r.next(), State::Action(_)));
    r.resume(json!(false), false).unwrap();
    assert_eq!(returned(r), json!("no"));
}

#[test]
fn javascript_truth_values_and_short_circuit_results_are_deterministic() {
    for (value, truth) in [
        (json!(null), false),
        (json!(false), false),
        (json!(0), false),
        (json!(0.0), false),
        (json!(-0.0), false),
        (json!(true), true),
        (json!(1), true),
        (json!(-1), true),
        (json!(0.25), true),
        (json!(""), false),
        (json!("false"), true),
        (json!([]), true),
        (json!({}), true),
    ] {
        for expression in [
            "input ? true : false",
            "!!input",
        ] {
            let source = format!("function main(input){{return {expression};}}");
            let r = start(&source, vec![value.clone()]);
            assert_eq!(returned(r), json!(truth), "{expression}: {value}");
        }
        let source = "function main(input){return input && true;}";
        let r = start(source, vec![value.clone()]);
        let normalized = if value.as_f64() == Some(0.0) { json!(0) } else { value.clone() };
        assert_eq!(returned(r), if truth { json!(true) } else { normalized.clone() });
        let source = "function main(input){return input || false;}";
        let r = start(source, vec![value.clone()]);
        assert_eq!(returned(r), if truth { normalized } else { json!(false) });
    }
    assert_eq!(
        returned(runtime(
            "function main(){const items=[1];const result={};const obj={};if(items.length && !result.error && obj){return true;}return false;}"
        )),
        json!(true)
    );
    assert_eq!(
        returned(runtime(
            "function main(){var i=3;var count=0;while(i){i--;count++;}return count;}"
        )),
        json!(3)
    );
    assert!(
        matches!(runtime("function main(){assert(null,'missing');}").next(),State::Failed(e) if e["message"]=="missing")
    );
}

#[test]
fn old_operator_enabled_runtime_cannot_resume_pending_effects() {
    let mut r = runtime("function main(){io.write({path:'not-replayed',content:'x'});}");
    assert!(matches!(r.next(), State::Action(_)));
    r.program.version = 1;
    let mut restored: Runtime = serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap();
    assert!(matches!(restored.next(),State::Failed(error) if error["required_version"]==VERSION));
}

#[test]
fn integer_literals_and_task_parameters_remain_json_integers() {
    let source = "function main(input){io.list({path:'.',max_depth:2,limit:input.limit});}";
    let mut r = start(source, vec![json!({"limit":3})]);
    let State::Action(action) = r.next() else {
        panic!()
    };
    let ActionKind::Tool { args, .. } = action.kind else {
        panic!()
    };
    assert_eq!(args["max_depth"].as_u64(), Some(2));
    assert_eq!(args["limit"].as_u64(), Some(3));
}

#[test]
fn programs_have_one_main_and_helpers_use_normal_returns() {
    assert!(compile("function prepare(){return 'ready';}").is_ok());
    assert!(compile("function main(){} function main(){}").is_err());
    let mut r = runtime(
        "function prepare(){return {ready:true};} function execute(input){io.write({path:'done.txt',content:'done'});return input;} function main(){const prepared=prepare();const result=execute(prepared);return result;}",
    );
    assert!(
        matches!(r.next(), State::Action(_)),
        "prepare returning must resume main and run execute"
    );
    r.resume(json!({"changed":true}), false).unwrap();
    assert_eq!(returned(r), json!({"ready":true}));
}

#[test]
fn comment_helpers_bind_arguments_and_resume_mixed_tool_library_calls() {
    let source = r#"
function main(request) {
  const original = io.read({path: request.source});
  const saved = process(original.content, request);
  return {saved, original: original.content};
}
function process(content, request) {
  // Explicit tool calls and task calls share one sequence.
  return io.write({path: request.output, content: transform(content, request).content});
}
/** @virtual */
function transform(content, request) {
  // Transform content according to request; preserve the requested scope.
  /* Return {content: string} with the transformed text. */
}
"#;
    let mut r = Runtime::new(
        compile(source).unwrap(),
        "main",
        vec![json!({"source":"source.txt","output":"out.txt"})],
    )
    .unwrap();
    assert!(
        matches!(r.next(),State::Action(Action{kind:ActionKind::Tool{name,args},..}) if name=="read_file" && args["path"]=="source.txt")
    );
    r.resume(json!({"content":"SOURCE"}), false).unwrap();
    let State::Action(task) = r.next() else {
        panic!()
    };
    assert!(
        matches!(task.kind,ActionKind::Call{name,args,..} if name=="transform" && args==vec![json!("SOURCE"),json!({"source":"source.txt","output":"out.txt"})])
    );
    let mut restored: Runtime = serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap();
    restored
        .resume(json!({"content":"CHANGED"}), false)
        .unwrap();
    assert!(
        matches!(restored.next(),State::Action(Action{kind:ActionKind::Tool{name,args},..}) if name=="write_file" && args==json!({"path":"out.txt","content":"CHANGED"}))
    );
    restored.resume(json!({"path":"out.txt"}), false).unwrap();
    assert_eq!(
        returned(restored),
        json!({"saved":{"path":"out.txt"},"original":"SOURCE"})
    );
}

#[test]
fn comment_errors_propagate_and_nested_calls_choose_boolean_branches() {
    let mut r = runtime(
        "function check(){/* @virtual */ // Return {passed:boolean} after inspecting actual artifacts.\n} function main(){try{if(check().passed){return 'ok';}else{throw 'bad';}}catch(error){return {error};}}",
    );
    assert!(matches!(
        r.next(),
        State::Action(Action {
            kind: ActionKind::Call { .. },
            ..
        })
    ));
    r.resume(json!({"passed":false}), false).unwrap();
    // A thrown string is caught as that string.
    assert_eq!(returned(r)["error"], json!("bad"));
    let mut r = runtime(
        "function work(){/* @virtual */ // Complete the assigned work.\n} function main(){try{return work();}catch(error){return error;}}",
    );
    assert!(matches!(r.next(), State::Action(_)));
    r.resume(json!({"message":"cannot complete"}), true)
        .unwrap();
    assert_eq!(returned(r)["message"], json!("cannot complete"));
    assert_eq!(returned(runtime("function main(){// only preparation\n}")),Value::Null);
    assert_eq!(returned(runtime("function main(){return blank();} function blank(){}")),Value::Null);
    assert!(compile("function main(){const $temp1=1;return $temp1;}").is_err());
    assert_eq!(
        returned(runtime("function main(){return '// this is data';}")),
        json!("// this is data")
    );
}


#[test]
fn local_result_arrays_collect_nested_task_returns_without_operators() {
    let source = "function main(){const rows=[];for(const item of [{n:1},{n:2}]){rows.push(process(item));}const length=rows.push({n:3});return {rows,length};} function process(item){return item;}";
    let r = start_task(compile(source).unwrap(), vec![]);
    let restored: Runtime = serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap();
    assert_eq!(
        returned(restored),
        json!({"rows":[{"n":1},{"n":2},{"n":3}],"length":3})
    );
    assert!(matches!(
        runtime("function main(){const x={};x.push(1);return x;}").next(),
        State::Failed(_)
    ));
    assert_eq!(
        returned(runtime("function main(){const x=[];x.push(1+2);return x;}")),
        json!([3])
    );
}

#[test]
fn ordinary_frames_suspend_for_virtual_work_and_survive_restore() {
    let source = r#"
function main(){const outer='OUTER';const result=work('INPUT');return {result,outer};}
function work(input){const local='LOCAL';const answer=think(input);return {answer,local};}
/** @virtual */ function think(input){/* Return the inspected result. */}
"#;
    let mut run = start_task(compile(source).unwrap(), vec![]);
    let State::Action(action) = run.next() else {
        panic!("expected virtual task")
    };
    assert!(
        matches!(&action.kind, ActionKind::Call { name, args, .. } if name == "think" && args == &vec![json!("INPUT")])
    );
    assert_eq!(run.named_bindings()["outer"], "OUTER");
    assert_eq!(run.named_bindings()["local"], "LOCAL");
    let mut restored: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    assert!(matches!(restored.next(), State::Action(ref pending) if pending.id == action.id));
    restored.resume(json!("DONE"), false).unwrap();
    assert_eq!(
        returned(restored),
        json!({"result":{"answer":"DONE","local":"LOCAL"},"outer":"OUTER"})
    );
}

#[test]
fn ordinary_frames_restore_tool_receipts_and_propagate_errors_to_caller() {
    let source = r#"
function main(){try{return helper();}catch(problem){return {error:problem.message};}}
function helper(){return io.write({path:'out.txt',content:'hello'});}
"#;
    let mut run = start_task(compile(source).unwrap(), vec![]);
    let State::Action(action) = run.next() else {
        panic!("expected write")
    };
    assert!(matches!(&action.kind, ActionKind::Tool { name, .. } if name == "write_file"));
    let saved = serde_json::to_value(run).unwrap();
    let mut success: Runtime = serde_json::from_value(saved.clone()).unwrap();
    assert!(matches!(success.next(), State::Action(ref pending) if pending.id == action.id));
    success.resume(json!({"path":"out.txt"}), false).unwrap();
    assert_eq!(returned(success), json!({"path":"out.txt"}));
    let mut failure: Runtime = serde_json::from_value(saved).unwrap();
    failure
        .resume(json!({"error":"write failed"}), true)
        .unwrap();
    assert_eq!(returned(failure), json!({"error":"write failed"}));
}

#[test]
fn inline_caller_bindings_preserve_inner_shadowing() {
    let source = r#"
function main(){const shared='OUTER';const result=helper();return {shared,result};}
function helper(){const shared='INNER';const result=think();return {shared,result};}
/** @virtual */ function think(){/* Return a result. */}
"#;
    let mut run = start_task(compile(source).unwrap(), vec![]);
    assert!(matches!(
        run.next(),
        State::Action(Action {
            kind: ActionKind::Call { .. },
            ..
        })
    ));
    assert_eq!(run.named_bindings()["shared"], "INNER");
    run.resume(json!("OK"), false).unwrap();
    assert_eq!(
        returned(run),
        json!({"shared":"OUTER","result":{"shared":"INNER","result":"OK"}})
    );
}

#[test]
fn version_eight_rejects_javascript_extensions_at_every_source_entry() {
    // Valid-looking code can precede invalid syntax: compilation must reject
    // the entire source, including unused functions, before any effects run.
    for fragment in [
        "class Hidden { #field=1; }",
        "yield io.read({});",
        "yield* rows;",
        "await io.read({});",
        "call(io.read,{});",
        "call('read_file',{});",
        "ai({instruction:'work'});",
        "const typed: string = 'x';",
        "const typed = value as number;",
        "const typed = <number>value;",
        "const typed = value!;",
        "const bigint = 1n;",
        "const infinity = 1e999;",
        "const infinity = value + 1e999;",
        "const hidden = data.check(1n);",
        "const runtime = process.exit();",
        "const runtime = data.check([,]);",
        "return import('module');",
    ] {
        let body = format!(
            "const data={{value:1}}; const rows=[]; let value=1; io.write({{path:'never-written'}});\n{fragment}"
        );
        for (source, mode) in [
            (
                format!("function main(){{{body}}} function helper(input){{return input;}}"),
                0,
            ),
            (
                format!("function work(){{{body}}} function helper(input){{return input;}}"),
                1,
            ),
            (
                format!("function work(){{{body}}} function helper(input){{return input;}}"),
                2,
            ),
        ] {
            let result = match mode {
                0 => compile(&source),
                1 => compile_body(&source, "work"),
                _ => compile_entry(&source, "work"),
            };
            let error = result.expect_err(&format!("must reject {fragment}, entry {mode}"));
            assert!(
                error.to_string().starts_with("TS_PROGRAM "),
                "{fragment}: {error}"
            );
        }
    }
    // Ordinary JavaScript operators and function values are accepted at every entry.
    for fragment in [
        "for(const key in data){}",
        "switch(value){case 1:break;}",
        "try{}catch(error){}finally{}",
        "const copy = {...data};",
        "const copy = [...[1,2]];",
        "Boolean(...[1]);",
        "async function hidden(){return 1;}",
        "async function* hidden(){yield 1;}",
        "function hidden(){return 1;}",
        "const fn = function(){return 1;};",
        "const fn = () => 1;",
        "const hidden = data.check(/x/);",
        "const runtime = data.check({run(){return 1;}});",
        "const missing = data?.value;",
        "const bits = value | 1;",
        "const bits = ~value;",
        "const power = value ** 2;",
        "void value;",
        "delete data.value;",
        "const runtime = 'value' in data;",
    ] {
        let body = format!("const data={{value:1}}; let value=1;\n{fragment}");
        let source = format!("function work(){{{body}}}");
        compile(&format!("function main(){{{body}}}")).unwrap_or_else(|e| panic!("{fragment}: {e}"));
        compile_body(&source, "work").unwrap_or_else(|e| panic!("{fragment}: {e}"));
        compile_entry(&source, "work").unwrap_or_else(|e| panic!("{fragment}: {e}"));
    }
    for source in [
        "class Hidden { #field=1; } function main(){return 1;}",
        "function main(){return 1;} class Unused { field=1; }",
        "import {tools} from '@agent/runtime'; function main(){return 1;}",
        "export function main(){return 1;}",
        "type Result = string; function main(){return 1;}",
        "interface Result{} function main(){return 1;}",
        "function main(input: string){return input;}",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}


#[test]
fn json_fields_and_elements_capture_values_before_later_array_mutation() {
    for (source, expected) in [
        (
            "function main(){const rows=[];return {before:rows,length:rows.push('item'),after:rows};}",
            json!({"before":["item"],"length":1,"after":["item"]}),
        ),
        (
            "function main(){const rows=[];return [rows,rows.push('item'),rows];}",
            json!([["item"], 1, ["item"]]),
        ),
        (
            "function main(){const rows=['old'];return rows[choose(rows.push('new'))];} function choose(length){return 1;}",
            json!("new"),
        ),
        (
            "function main(){let result='before';result='after';return result;}",
            json!("after"),
        ),
    ] {
        assert_eq!(returned(runtime(source)), expected, "{source}");
    }
}

#[test]
fn all_older_program_versions_reject_pending_effects_without_replaying() {
    for version in 1..VERSION {
        let mut r = runtime("function main(){io.write({path:'never-replayed'});}");
        assert!(matches!(r.next(), State::Action(_)));
        r.program.version = version;
        let mut restored: Runtime =
            serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap();
        assert!(matches!(restored.next(), State::Failed(_)));
        assert!(restored.pending.is_none());
    }
}

#[test]
fn embedded_skill_and_tool_contracts_advertise_the_same_source_whitelist() {
    for text in [SKILL.to_owned(), CAPABILITIES.to_owned()] {
        for required in [
            "function*",
            "yield",
            "Promise.all",
            "call()/ai()",
            "if/else",
        ] {
            assert!(text.contains(required), "missing {required}");
        }
    }
}

#[test]
fn recorded_twenty_page_parallel_example_compiles_with_declared_host_tools() {
    let source = include_str!("../../skills/task-program/examples/parallel-twenty-html.js");
    let program =
        compile(source).expect("recorded example must compile against its declared tools");
    assert!(program.functions["designStyles"].virtual_task);
    assert!(program.functions["generatePageBody"].virtual_task);
    assert!(program.functions["generatePageBody"].is_async);
    assert!(!program.functions["renderPage"].virtual_task);
}

#[test]
fn published_flow_examples_compile_with_the_fixed_whitelist() {
    for text in [SKILL, include_str!("../../docs/agent-program.md")] {
        let mut examples = 0;
        let text = text.replace("\r\n", "\n");
        for block in text.split("```js\n").skip(1) {
            let source = block.split("```").next().unwrap();
            let result =
                if source.contains("function process(") && !source.contains("function main(") {
                    compile_body(source, "process")
                } else {
                    compile(source)
                };
            result.unwrap_or_else(|error| panic!("published example failed: {error}\n{source}"));
            examples += 1;
        }
        assert!(examples >= 2);
    }
}

// Run the published files themselves. Tool/AI observations are controlled here;
// this verifies flow semantics, not real model quality or wall-clock parallelism.
#[derive(Default)]
struct TutorialRun {
    files: BTreeMap<String, String>,
    events: Vec<String>,
    workers: BTreeMap<String, Runtime>,
    reject_review: bool,
    /// A rejected page passes once it has been revised.
    accept_revised: bool,
}

impl TutorialRun {
    fn run(&mut self, mut run: Runtime) -> Result<Value, Value> {
        loop {
            let action = match run.next() {
                State::Returned(value) => return Ok(value),
                State::Failed(error) => return Err(error),
                State::Waiting => panic!("serial tutorial cannot have concurrent calls"),
                State::Action(action) => action,
            };
            let result = match action.kind {
                ActionKind::ParallelReads { .. } => panic!("tutorial does not use parallel reads"),
                ActionKind::Require { .. } => panic!("tutorial does not require libraries"),
                ActionKind::Call { name, args, .. } => {
                    self.run(Runtime::task_program(run.program.clone(), &name, args).unwrap())
                }
                ActionKind::Tool { name, args } => {
                    let path = args["path"].as_str().unwrap();
                    self.events.push(format!("{name}:{path}"));
                    match name.as_str() {
                        "read_file" => Ok(json!({"content":self.files[path]})),
                        "write_file" => {
                            self.files
                                .insert(path.into(), args["content"].as_str().unwrap().into());
                            Ok(json!({"changed":true}))
                        }
                        _ => panic!("unexpected tutorial tool {name}"),
                    }
                }
                ActionKind::Task { name, input, .. } => {
                    self.events.push(name.clone());
                    match name.as_str() {
                        "compose" => {
                            assert!(input["request"]["topic"].is_string());
                            Ok(json!({"content":"<html><body>串行示例</body></html>"}))
                        }
                        "designPage" => {
                            assert_eq!(input["shared"]["source_content"], "SOURCE");
                            assert!(input["shared"]["requirements"].is_string());
                            let title = input["page"]["title"].as_str().unwrap();
                            Ok(json!({"content":format!("<html><body>{title}</body></html>")}))
                        }
                        "inspect" | "inspectPage" => {
                            let content = input["content"].as_str().unwrap();
                            assert!(
                                self.files.values().any(|value| value == content),
                                "must inspect actual saved content"
                            );
                            if name == "inspectPage" {
                                let path = input["page"]["path"].as_str().unwrap();
                                assert_eq!(self.files[path], content);
                                assert_eq!(input["shared"]["source_content"], "SOURCE");
                                assert!(content.contains(input["page"]["title"].as_str().unwrap()));
                            }
                            let passed = !self.reject_review || (self.accept_revised && content.contains("已修正"));
                            Ok(
                                json!({"passed":passed,"issues":if passed {json!([])} else {json!(["缺少要点"])},"evidence":[content]}),
                            )
                        }
                        "revisePage" => {
                            let path = input["page"]["path"].as_str().unwrap();
                            assert_eq!(self.files[path], input["content"], "revise the saved page");
                            assert_eq!(input["issues"], json!(["缺少要点"]));
                            Ok(json!({"content":format!("{}（已修正）", input["content"].as_str().unwrap())}))
                        }
                        "summarizeFile" => {
                            assert!(input["requirements"].is_string(), "a module gets the requirements as a parameter");
                            let path = input["path"].as_str().unwrap();
                            Ok(json!({"path":path,"points":[format!("{path} 的要点")]}))
                        }
                        _ => panic!("unexpected tutorial task {name}"),
                    }
                }
                ActionKind::Fork { entry, args } => {
                    assert_eq!(entry, "makePages");
                    assert_eq!(
                        args.len(),
                        2,
                        "shared and pages are separate positional arguments"
                    );
                    let id = format!("worker-{}", self.workers.len());
                    self.events
                        .push(format!("fork:{}", args[1].as_array().unwrap().len()));
                    self.workers.insert(
                        id.clone(),
                        Runtime::task_program(run.program.clone(), &entry, args).unwrap(),
                    );
                    Ok(
                        json!({"agent_id":id,"workspace":{"mode":"shared","integration_required":false}}),
                    )
                }
                ActionKind::Join { ids } => {
                    self.events.push("join".into());
                    let mut outputs = Vec::new();
                    for id in ids {
                        let worker = self.workers.remove(&id).unwrap();
                        outputs.push(self.run(worker)?);
                    }
                    Ok(Value::Array(outputs))
                }
            };
            let failed = result.is_err();
            run.resume(result.unwrap_or_else(|error| error), failed)
                .unwrap();
        }
    }
}

fn tutorial(source: &str) -> Runtime {
    assert!(
        SKILL.contains(source.trim()),
        "the AI must receive the same example that is tested"
    );
    Runtime::task_program(compile(source).unwrap(), crate::agent_program::ENTRY, vec![]).unwrap()
}

#[test]
fn documented_serial_example_writes_reads_and_checks_without_fork() {
    let mut harness = TutorialRun::default();
    let output = harness
        .run(tutorial(include_str!("../../skills/task-program/examples/serial.js")))
        .unwrap();
    assert_eq!(output, Value::Null);
    assert!(harness.files.contains_key("agent-overview.html"));
    assert_eq!(
        harness.events,
        vec![
            "compose",
            "write_file:agent-overview.html",
            "read_file:agent-overview.html",
            "inspect"
        ]
    );
    assert!(harness.workers.is_empty());
}

#[test]
fn documented_parallel_example_checks_all_five_pages() {
    let mut harness = TutorialRun::default();
    harness
        .files
        .insert("docs/agent-design-guide.md".into(), "SOURCE".into());
    let output = harness
        .run(tutorial(include_str!("../../skills/task-program/examples/parallel-five-pages.js")))
        .unwrap();
    assert_eq!(harness.events.first().unwrap(), "read_file:docs/agent-design-guide.md");
    assert_eq!(output, Value::Null);
    assert_eq!(harness.files.len(), 6); // one source plus five distinct outputs
    assert_eq!(
        harness.events.iter().filter(|e| *e == "designPage").count(),
        5
    );
    assert_eq!(
        harness
            .events
            .iter()
            .filter(|e| *e == "inspectPage")
            .count(),
        5
    );
    for path in harness.files.keys().filter(|path| path.ends_with(".html")) {
        let path = path.as_str();
        for operation in ["write_file", "read_file"] {
            assert_eq!(
                harness
                    .events
                    .iter()
                    .filter(|e| **e == format!("{operation}:{path}"))
                    .count(),
                1
            );
        }
    }
    assert!(harness.workers.is_empty());
}

#[test]
fn documented_module_example_summarizes_each_file_with_one_module() {
    let source = include_str!("../../skills/task-program/examples/module-notes.js");
    assert!(compile(source).unwrap().functions["summarizeFile"].module_task);
    let mut harness = TutorialRun::default();
    let output = harness.run(tutorial(source)).unwrap();
    assert_eq!(output, Value::Null);
    assert_eq!(harness.events.iter().filter(|e| *e == "summarizeFile").count(), 3);
    assert_eq!(harness.events.iter().filter(|e| *e == "write_file:notes.md").count(), 1);
    assert_eq!(
        harness.files["notes.md"],
        "## README.md\n- README.md 的要点\n\n## docs/agent-design-guide.md\n- docs/agent-design-guide.md 的要点\n\n## docs/agent-program.md\n- docs/agent-program.md 的要点"
    );
}

#[test]
fn documented_examples_fail_instead_of_delivering_failed_reviews() {
    for source in [
        include_str!("../../skills/task-program/examples/serial.js"),
        include_str!("../../skills/task-program/examples/parallel-five-pages.js"),
    ] {
        let mut harness = TutorialRun {
            reject_review: true,
            ..Default::default()
        };
        harness
            .files
            .insert("docs/agent-design-guide.md".into(), "SOURCE".into());
        let error = harness.run(tutorial(source)).unwrap_err();
        assert!(error["message"].as_str().unwrap().contains("验收"));
        assert_eq!(
            harness
                .events
                .iter()
                .filter(|e| matches!(e.as_str(), "inspect" | "inspectPage"))
                .count(),
            if source.contains("function inspectPage(") { 10 } else { 1 }
        );
    }
}

#[test]
fn the_parallel_example_revises_rejected_pages_and_rechecks_them() {
    let mut harness = TutorialRun { reject_review: true, accept_revised: true, ..Default::default() };
    harness.files.insert("docs/agent-design-guide.md".into(), "SOURCE".into());
    let output = harness
        .run(tutorial(include_str!("../../skills/task-program/examples/parallel-five-pages.js")))
        .unwrap();
    assert_eq!(output, Value::Null);
    assert_eq!(harness.events.iter().filter(|e| *e == "revisePage").count(), 5);
    assert_eq!(harness.events.iter().filter(|e| *e == "inspectPage").count(), 10);
    assert!(harness.files.iter().filter(|(path, _)| path.ends_with(".html")).all(|(_, content)| content.contains("已修正")));
}

#[test]
fn common_loops_updates_templates_and_assignment_order_are_deterministic() {
    for (source, expected) in [
        (
            "function main(){var sum=0;for(var i=0;i<5;i++){if(i===1)continue;if(i===4)break;sum+=i;}return {sum,i};}",
            json!({"sum":5,"i":4}),
        ),
        (
            "function main(){var i=0;var sum=0;while(i<4){i++;if(i===2)continue;sum+=i;}return sum;}",
            json!(8),
        ),
        (
            "function main(){var i=0;do{i++;if(i<3)continue;}while(i<3);return i;}",
            json!(3),
        ),
        (
            "function main(){var count=0;for(var i=0;i<3;i++){for(var j=0;j<3;j++){if(j===1)break;count++;}}return count;}",
            json!(3),
        ),
        (
            "function main(){var rows=[];for(var item of [1,2,3]){if(item===2)continue;rows.push(item);}return rows;}",
            json!([1, 3]),
        ),
        (
            "function main(){var i=1;var rows=[i++,++i,i--,--i];return {rows,i};}",
            json!({"rows":[1,3,3,1],"i":1}),
        ),
        (
            "function main(){var x='before';return {first:x,second:(x='after'),third:x};}",
            json!({"first":"before","second":"after","third":"after"}),
        ),
        (
            "function main(){var x=1;return `${x++}-${x}-${x+=2}`;}",
            json!("1-2-4"),
        ),
        (
            "function main(){var value;var value=2;var value;return value;}",
            json!(2),
        ),
        (
            "function main(){var value=0;for(;;){value++;if(value===3)break;}return value;}",
            json!(3),
        ),
    ] {
        for _ in 0..3 {
            assert_eq!(returned(runtime(source)), expected, "{source}");
        }
    }
}

#[test]
fn lazy_expressions_execute_only_selected_effects_once() {
    for source in [
        "function main(){return false && test.never({});}",
        "function main(){return true || test.never({});}",
        "function main(){return true ? 'selected' : test.never({});}",
        "function main(){return false ? test.never({}) : 'selected';}",
        "function main(){return 'selected' ?? test.never({});}",
    ] {
        assert!(
            matches!(runtime(source).next(), State::Returned(_)),
            "{source}"
        );
    }
    for expression in [
        "true && test.selected({})",
        "false || test.selected({})",
        "null ?? test.selected({})",
        "true ? test.selected({}) : test.never({})",
    ] {
        let mut r = runtime(&format!("function main(){{return {expression};}}"));
        let State::Action(action) = r.next() else {
            panic!("{expression}")
        };
        assert!(matches!(action.kind,ActionKind::Tool{name,..} if name=="selected"));
        let mut restored: Runtime =
            serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap();
        restored.resume(json!(true), false).unwrap();
        assert_eq!(returned(restored), json!(true));
    }
    // Division by zero is not an exception in JavaScript; the later call runs.
    assert!(matches!(runtime("function main(){return (1/0)+test.never({});}").next(), State::Action(_)));
}

#[test]
fn loop_jumps_unwind_only_the_handlers_they_leave() {
    for source in [
        "function main(){try{for(var i=0;i<2;i++){try{if(i===0)continue;break;}catch(inner){return 'wrong';}}throw 'outer';}catch(outer){return outer;}}",
        "function main(){try{while(true){try{break;}catch(inner){return 'wrong';}}throw 'outer';}catch(outer){return outer;}}",
        "function main(){try{for(var item of [1,2]){try{if(item===1)continue;break;}catch(inner){return 'wrong';}}throw 'outer';}catch(outer){return outer;}}",
        "function main(){try{do{try{break;}catch(inner){return 'wrong';}}while(true);throw 'outer';}catch(outer){return outer;}}",
    ] {
        assert_eq!(returned(runtime(source)), json!("outer"), "{source}");
    }
    assert!(
        matches!(runtime("function main(){while(true){try{break;}catch(inner){return 'leaked';}}throw 'outside';}").next(),State::Failed(e) if e["message"]=="outside")
    );
    assert!(
        matches!(runtime("function main(){try{while(true){}}catch(error){return 'cannot hide exhaustion';}}").next(),State::Failed(e) if e["code"]=="INSTRUCTION_BUDGET_EXHAUSTED" && e["used"]==1_000_000)
    );
}

#[test]
fn suspended_loops_do_not_repeat_updates_or_effects_after_restore() {
    let mut r = runtime(
        "function main(){var rows=[];for(var i=0;i<3;i++){var next=i+1;const receipt=test.record({i,next});rows.push(receipt);}return {rows,i};}",
    );
    for i in 0..3 {
        let State::Action(action) = r.next() else {
            panic!()
        };
        assert!(matches!(action.kind,ActionKind::Tool{args,..} if args==json!({"i":i,"next":i+1})));
        r = serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap();
        assert!(matches!(r.next(),State::Action(ref again) if again.id==action.id));
        r.resume(json!(i), false).unwrap();
    }
    assert_eq!(returned(r), json!({"rows":[0,1,2],"i":3}));
}

#[test]
fn no_implicit_semantic_effects_are_emitted_or_executed() {
    let program =
        compile("function main(){var total=0;for(var i=0;i<4;i++){total+=i;}return `${total}`;}")
            .unwrap();
    assert!(
        program
            .functions
            .iter()
            .filter(|(name, _)| !name.starts_with("$$"))
            .flat_map(|(_, f)| &f.code)
            .all(|i| match &i.op {
                // Coercions may run explicit user hooks, but this pure source
                // must never acquire an implicit host tool or AI effect.
                Op::Effect(_,Effect::Dynamic{callee:Expr::Native(name),..}) if name=="$$coercePrimitive"=>true,
                Op::Effect(_,_)=>false,_=>true,
            })
    );
    let mut runtime = start_task(program, vec![]);
    assert!(matches!(runtime.next(),State::Returned(v) if v=="6"));
}

#[test]
fn library_calls_compile_to_exact_tools_and_keep_json_arguments() {
    let mut bindings = libraries::builtin();
    bindings.insert(
        "ppt".into(),
        BTreeMap::from([("get_design".into(), "presentation__get_design".into())]),
    );
    for (source, name, args) in [
        (
            "function main(){return io.read({path:'source.txt'});}",
            "read_file",
            json!({"path":"source.txt"}),
        ),
        (
            "function main(){return ppt.get_design({project:'actual'});}",
            "presentation__get_design",
            json!({"project":"actual"}),
        ),
    ] {
        let mut r = Runtime::new(
            compile_with_libraries(source, bindings.clone()).unwrap(),
            "main",
            vec![],
        )
        .unwrap();
        let State::Action(action) = r.next() else {
            panic!()
        };
        assert!(
            matches!(action.kind,ActionKind::Tool{name:ref target,args:ref actual} if target==name && actual==&args)
        );
        let mut restored: Runtime =
            serde_json::from_value(serde_json::to_value(r).unwrap()).unwrap();
        restored.resume(json!({"actual":"receipt"}), false).unwrap();
        assert_eq!(returned(restored), json!({"actual":"receipt"}));
    }
    for source in [
        "function main(){io.write({path:'never'});return io.unknown({});}",
        "function main(){return ppt.get_design({});}",
        "function main(){return io['read']({});}",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    // A variable the source declares is its own, even named like a library.
    assert_eq!(returned(runtime("function main(){const io={};return io;}")), json!({}));
}

#[test]
fn library_catalog_uses_real_ownership_and_omits_unavailable_or_ambiguous_names() {
    let definitions = ["read_file", "ppt_master__get_design", "other__inspect"]
        .map(|name| json!({"function":{"name":name}}));
    let packages = json!([
        {"id":"ppt-master","library":"ppt","enabled":true,"tools":["ppt_master__get_design","ppt_master__missing"]},
        {"id":"other","enabled":false,"tools":["other__inspect"]},
    ]);
    let catalog = libraries::catalog(&definitions, &packages);
    assert_eq!(catalog["io"]["read"], "read_file");
    assert!(!catalog["io"].contains_key("write"));
    assert_eq!(catalog["ppt"]["get_design"], "ppt_master__get_design");
    assert!(!catalog["ppt"].contains_key("missing"));
    assert!(!catalog.contains_key("other"));
    let collision = json!([
        {"id":"a-b","enabled":true,"tools":["read_file"]},
        {"id":"a_b","enabled":true,"tools":["other__inspect"]},
    ]);
    assert!(!libraries::catalog(&definitions, &collision).contains_key("a_b"));
}

#[test]
fn require_links_a_library_when_its_line_runs_and_keeps_one_object() {
    let source = r#"function main(){
  const before = io.read({path:'first'});
  const ppt = require("ppt");
  const same = require("ppt") === ppt;
  const design = ppt.get_design({page: 2});
  let missing = null;
  try { require("absent"); } catch (error) { missing = error.code; }
  let unknown = null;
  try { ppt.nothing({}); } catch (error) { unknown = error.message; }
  return {before: before.content, same, design, missing, unknown};
}"#;
    let mut run = runtime(source);
    assert!(matches!(run.next(), State::Action(Action{kind:ActionKind::Tool{name,..},..}) if name=="read_file"), "nothing links before its line runs");
    run.resume(json!({"content":"read"}), false).unwrap();
    let State::Action(action) = run.next() else { panic!("require asks the host") };
    assert!(matches!(&action.kind, ActionKind::Require { library } if library == "ppt"));
    // A pending link survives a snapshot like any other action.
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"library":"ppt","methods":{"get_design":"ppt_master__get_design"}}), false).unwrap();
    let State::Action(action) = run.next() else { panic!("the method runs its tool") };
    assert!(
        matches!(&action.kind, ActionKind::Tool { name, args } if name == "ppt_master__get_design" && args == &json!({"page":2})),
        "requiring the library again asked the host nothing: {action:?}"
    );
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"slides":2}), false).unwrap();
    let State::Action(action) = run.next() else { panic!("another library links at its own line") };
    assert!(matches!(&action.kind, ActionKind::Require { library } if library == "absent"));
    run.resume(json!({"code":"MODULE_NOT_FOUND","message":"Cannot find library 'absent'"}), true).unwrap();
    let result = returned(run);
    assert_eq!(result["before"], "read");
    assert_eq!(result["same"], true);
    assert_eq!(result["design"], json!({"slides":2}));
    assert_eq!(result["missing"], "MODULE_NOT_FOUND", "a library that cannot be found throws at its require line");
    assert!(result["unknown"].as_str().unwrap().contains("nothing"), "a method the library lacks fails where it is called: {result}");
}

#[test]
fn a_required_library_outlives_collection_and_its_promises_variant_is_async() {
    // Nothing but the run's cache holds the first object while the loop runs
    // long enough to collect the heap.
    let source = r#"function main(){
  require("ppt");
  let total = 0;
  for (let i = 0; i < 400; i++) { total += i; }
  const ppt = require("ppt");
  return [typeof ppt.get_design, total];
}"#;
    let mut run = runtime(source);
    let State::Action(_) = run.next() else { panic!("require asks the host") };
    run.resume(json!({"library":"ppt","methods":{"get_design":"ppt_master__get_design"}}), false).unwrap();
    assert_eq!(returned(run), json!(["function", 79800]));
    let source = r#"async function main(){
  const ppt = require("ppt/promises");
  const pending = ppt.get_design({page: 3});
  const plain = require("ppt");
  return [(await pending).slides, plain === ppt];
}"#;
    let mut run = runtime(source);
    let State::Action(action) = run.next() else { panic!("require asks the host") };
    assert!(matches!(&action.kind, ActionKind::Require { library } if library == "ppt/promises"));
    run.resume(json!({"library":"ppt/promises","methods":{"get_design":"ppt_master__get_design"}}), false).unwrap();
    let State::Action(action) = run.next() else { panic!("the sync variant is another library object") };
    assert!(matches!(&action.kind, ActionKind::Require { library } if library == "ppt"), "the async call is queued behind it: {action:?}");
    run.resume(json!({"library":"ppt","methods":{"get_design":"ppt_master__get_design"}}), false).unwrap();
    let State::Action(action) = run.next() else { panic!("the async method runs its tool") };
    assert!(matches!(&action.kind, ActionKind::Tool { name, .. } if name == "ppt_master__get_design"));
    run.resume(json!({"slides":3}), false).unwrap();
    assert_eq!(returned(run), json!([3, false]));
}

#[test]
fn require_takes_a_library_name_and_gives_its_methods_to_the_hooks_of_that_source() {
    for source in [
        "function main(){const name='ppt';return require(name);}",
        "function main(){return require('ppt', 1);}",
        "function main(){return require('host:ppt');}",
        "function main(){return require('a b');}",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
    let error = compile_with_libraries("import * as ppt from \"host:ppt\";\nppt.get_design({});", libraries::builtin()).unwrap_err();
    assert!(format!("{error:#}").contains("const ppt = require(\"ppt\")"), "the host: error says how to load it: {error:#}");
    let program = compile(r#"const ppt = require("ppt/promises");
/** @virtual Judge the deck and return {passed:boolean}. */
function judge(deck){}
/** @module Build the deck and return {pages:number}. */
function build(deck){}
judge(ppt);"#).unwrap();
    assert_eq!(program.functions["judge"].leaf_libraries, ["ppt"], "a hook gets the libraries its declaring source requires");
    assert!(program.functions["build"].leaf_libraries.is_empty(), "a module requires its own");
    // A declared function named require is an ordinary call.
    assert_eq!(returned(runtime("function require(name){return 'own ' + name;} function main(){return require('ppt');}")), json!("own ppt"));
}

#[test]
fn a_name_the_source_binds_shadows_a_library_it_did_not_import() {
    // Without imports the host's libraries are in scope by name, but a
    // variable the source declares is its own, as in plain JavaScript.
    let mut run = runtime("const io = require(\"io\");\nconst text = io.read({path: \"a\"});\nconst echo = io.echo({});");
    let State::Action(action) = run.next() else { panic!("io is required at its line") };
    assert!(matches!(&action.kind, ActionKind::Require { library } if library == "io"));
    run.resume(json!({"library":"io","methods":{"read":"read_file"}}), false).unwrap();
    assert!(matches!(run.next(), State::Action(Action{kind:ActionKind::Tool{name,..},..}) if name == "read_file"), "its method is the required object's");
    run.resume(json!({"content":"x"}), false).unwrap();
    assert!(matches!(run.next(), State::Failed(error) if error["message"].as_str().unwrap().contains("echo")), "a method the object lacks fails at its call, not at compile time");
    // With an explicit import the name is taken, as in JavaScript.
    assert!(compile("import * as io from \"host:io\";\nconst io = require(\"io\");").is_err());
    // A source that binds nothing keeps the library by name.
    assert!(compile("const text = io.read({path: \"a\"});").is_ok());
}

#[test]
fn numeric_and_null_short_circuits_return_operands_and_skip_effects() {
    for (expression, expected) in [
        ("0 && test.never({})", json!(0)),
        ("null && test.never({})", json!(null)),
        ("{} || test.never({})", json!({})),
        ("1 || test.never({})", json!(1)),
        ("0 ?? test.never({})", json!(0)),
        ("!({}).error", json!(true)),
        ("0 || 7", json!(7)),
        ("1 && {}", json!({})),
    ] {
        assert_eq!(
            returned(runtime(&format!("function main(){{return {expression};}}"))),
            expected,
            "{expression}"
        );
    }
    let mut r = runtime("function main(){return 0 || io.read({path:'selected'});}");
    assert!(
        matches!(r.next(),State::Action(Action{kind:ActionKind::Tool{name,..},..}) if name=="read_file")
    );
    r.resume(json!({}), false).unwrap();
    assert_eq!(returned(r), json!({}));
}

#[test]
fn raw_tool_spellings_are_rejected_before_any_effect() {
    for body in [
        "return tools.read_file({path:'x'});",
        "return tools['read_file']({});",
        "return invoke_library({method:'io.read',args:{}});",
    ] {
        for source in [
            format!("function main(){{io.write({{path:'never'}});{body}}}"),
            format!("function main(){{return null;}} function unused(){{{body}}}"),
        ] {
            assert!(super::compile(&source).is_err(), "{source}");
        }
    }
}

#[test]
fn explicit_virtual_contract_runs_ai_and_preserves_result_across_restore() {
    let source = r#"
function main(request) { return inspect(request); }
/**
 * @virtual
 * Read actual evidence for request; return {passed, evidence}.
 */
function inspect(request) {}
"#;
    let program = compile(source).unwrap();
    assert!(program.functions["inspect"].virtual_task);
    assert!(!program.functions["main"].virtual_task);
    let mut run = Runtime::new(program, "main", vec![json!({"path":"real.txt"})]).unwrap();
    let State::Action(action) = run.next() else {
        panic!("must enter AI work")
    };
    assert!(
        matches!(action.kind, ActionKind::Call { ref name, ref args, .. }
        if name == "inspect" && args[0]["path"] == "real.txt")
    );
    let mut restored: Runtime =
        serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    let State::Action(pending) = restored.next() else {
        panic!()
    };
    assert_eq!(pending.id, action.id);
    let actual = json!({"passed":true,"evidence":[{"path":"real.txt","content":"actual"}]});
    restored.resume(actual.clone(), false).unwrap();
    assert_eq!(returned(restored), actual);
}


#[test]
fn virtual_tags_are_local_and_concrete_refinement_can_call_another_virtual_task() {
    let source = r#"
function main() {return concrete();}
/** @virtual Read the real evidence; return a JSON result. */
function inspect() {}
function concrete() { /* A normal code comment. */ return {status:'computed'}; }
"#;
    assert_eq!(returned(runtime(source)), json!({"status":"computed"}));
    // A word that merely starts with the tag is not a declaration marker.
    assert_eq!(
        returned(runtime(
            "function main(){/* @virtualized is ordinary prose. */return 3;}"
        )),
        json!(3)
    );
    let concrete = compile_body(
        r#"
function phase(input) {return inspect(input);}
/** @virtual Inspect actual input; return {passed,evidence}. */
function inspect(input) {}
"#,
        "phase",
    )
    .unwrap();
    assert!(!concrete.functions["phase"].virtual_task);
    assert!(concrete.functions["inspect"].virtual_task);
    assert!(
        compile_body(
            "/** @virtual Restate work and return results. */ function phase(){}",
            "phase"
        )
        .is_err()
    );
}

#[test]
fn string_concatenation_accepts_primitives_and_preserves_numeric_addition_order() {
    for (expression, expected) in [
        ("'字符数=' + 8000", json!("字符数=8000")),
        ("8000 + '字'", json!("8000字")),
        ("'数值=' + -2.5", json!("数值=-2.5")),
        ("'状态=' + true + '/' + null", json!("状态=true/null")),
        ("1 + 2 + '项'", json!("3项")),
        ("'项' + 1 + 2", json!("项12")),
        (
            "'大整数=' + 9007199254740993",
            json!("大整数=9007199254740992"),
        ),
        (
            "18446744073709551615 + '字'",
            json!("18446744073709552000字"),
        ),
    ] {
        assert_eq!(
            returned(runtime(&format!("function main(){{return {expression};}}"))),
            expected
        );
    }
    assert_eq!(
        returned(runtime(
            "function main(){let label='数量=';label+=2;let total=3;total+=2;return {label,total};}"
        )),
        json!({"label":"数量=2","total":5})
    );
    // Objects and arrays convert to their string forms, as in JavaScript.
    assert_eq!(
        returned(runtime("function main(){return ['x'+{},[]+'x',[1,[2,3]]+''];}")),
        json!(["x[object Object]", "x", "1,2,3"])
    );
    assert_eq!(returned(runtime("function main(){return [true+1,'3'-1,'3'<4];}")), json!([2,2,true]));
}

#[test]
fn captured_receipt_concatenation_allows_both_writes_and_resumes_without_replay() {
    let source = include_str!("fixtures/receipt-string-number.js");
    let program = compile_body(source, "build_pages").unwrap();
    let mut run = Runtime::new(
        program,
        "build_pages",
        vec![json!({"assignment":{"owner":"worker"},"shared":{}})],
    )
    .unwrap();
    assert!(
        matches!(run.next(),State::Action(Action{kind:ActionKind::Call{name,..},..}) if name=="create_page_content")
    );
    run.resume(json!({"pages":[{"path":"one.html","title":"One","content":"first"},{"path":"two.html","title":"Two","content":"second"}]}),false).unwrap();
    for path in ["one.html", "two.html"] {
        let State::Action(action) = run.next() else {
            panic!("both writes must be reached")
        };
        assert!(
            matches!(action.kind,ActionKind::Tool{name,args} if name=="write_file" && args["path"]==path)
        );
        run.resume(json!({"path":path,"changed":true}), false)
            .unwrap();
        // The second iteration must preserve the first receipt after recovery.
        run = serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    }
    let result = returned(run);
    assert_eq!(result["files"].as_array().unwrap().len(), 2);
    assert!(
        result["files"][0]["evidence"]
            .as_str()
            .unwrap()
            .ends_with("字符数=5")
    );
    assert!(
        result["files"][1]["evidence"]
            .as_str()
            .unwrap()
            .ends_with("字符数=6")
    );
}

#[test]
fn caller_can_reject_an_inspection_result_for_a_different_input() {
    let source = "function main(){const subject={id:'correct'};const result=inspect(subject);assert(result.id===subject.id,'result does not match its input');return result;} /** @virtual Return the inspected id and conclusion. */ function inspect(subject){}";
    let mut run = start_task(compile(source).unwrap(), vec![]);
    assert!(matches!(run.next(), State::Action(_)));
    run.resume(json!({"id":"other","passed":true}), false)
        .unwrap();
    assert!(
        matches!(run.next(), State::Failed(error) if error["message"] == "result does not match its input")
    );
}

#[test]
fn functions_share_module_variables_and_cannot_see_their_callers_locals() {
    // A top-level function and the top-level flow share one module binding.
    let source = "let count = 0; function bump(){ count++; return count; } bump(); bump(); test.record({count});";
    let mut run = start(source, vec![json!(null)]);
    assert!(matches!(
        run.next(),
        State::Action(Action { kind: ActionKind::Tool { name, args }, .. })
            if name == "record" && args == json!({"count": 2})
    ));
    // A caller's local variable is not in the called function's scope.
    for source in [
        "function main(){const wanted='YES';return helper();} function helper(){return wanted;}",
        "function main(){return helper();} function other(){const wanted='wrong scope';return wanted;} function helper(){return wanted;}",
    ] {
        let error = compile(source).unwrap_err().to_string();
        assert!(error.contains("unknown variable"), "{error}");
    }
}

#[test]
fn block_scopes_allow_repeated_names_and_shadowing() {
    let source = r#"
function main(){
  const rows = [];
  for (let i = 0; i < 2; i++) { rows.push(i); }
  for (let i = 10; i < 12; i++) { rows.push(i); }
  let x = 'outer';
  { let x = 'inner'; rows.push(x); }
  rows.push(x);
  try { throw new Error('a'); } catch (error) { rows.push(error.message); }
  try { throw new Error('b'); } catch (error) { rows.push(error.message); }
  for (const item of ['p', 'q']) { const label = item + '!'; rows.push(label); }
  return rows;
}"#;
    assert_eq!(
        returned(runtime(source)),
        json!([0, 1, 10, 11, "inner", "outer", "a", "b", "p!", "q!"])
    );
    for source in [
        "function main(){let a=1;let a=2;return a;}",
        "function main(){const a=1;var a=2;return a;}",
        "function main(p){let p=1;return p;}",
        "function main(){try{}catch(e){let e=1;}return 1;}",
        "function main(){undeclared=1;return 1;}",
    ] {
        assert!(compile(source).is_err(), "should reject {source}");
    }
}

#[test]
fn let_and_const_have_a_dead_zone_and_var_is_hoisted() {
    let source = r#"
function main(){
  const result = {};
  try { result.before = value; let value = 1; } catch (error) { result.tdz = error.name; }
  result.hoisted = typeof later;
  var later = 5;
  result.after = later;
  if (true) { var inner = 'block'; }
  result.inner = inner;
  var later;
  result.kept = later;
  return result;
}"#;
    assert_eq!(
        returned(runtime(source)),
        json!({"tdz": "ReferenceError", "hoisted": "undefined", "after": 5, "inner": "block", "kept": 5})
    );
}

#[test]
fn jumps_and_throws_leave_exactly_the_scopes_they_cross() {
    // At the final effect only `total` and `rows` are in scope: nothing from
    // the loop bodies or try blocks remains after break, continue or throw.
    let source = r#"
function main(){
  let total = 0;
  const rows = [];
  for (let i = 0; i < 5; i++) {
    const double = i * 2;
    if (i === 1) { const skip = true; continue; }
    if (i === 3) { const stop = true; break; }
    total += double;
  }
  for (const item of [1, 2]) { const seen = item; if (item === 1) continue; break; }
  while (true) { const once = 1; break; }
  try { const a = 1; { const b = 2; throw new Error('x'); } } catch (error) { rows.push(error.message); }
  test.record({total, rows});
  return total;
}"#;
    let mut run = runtime(source);
    assert!(matches!(
        run.next(),
        State::Action(Action { kind: ActionKind::Tool { args, .. }, .. })
            if args == json!({"total": 4, "rows": ["x"]})
    ));
    assert_eq!(
        run.named_bindings().keys().cloned().collect::<Vec<_>>(),
        ["rows", "total"]
    );
}

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
    let source = include_str!("fixtures/model-five-page-acceptance.js");
    let program = compile(source).unwrap();
    assert!(program.functions["proposeMinimalEdits"].virtual_task);
}

#[test]
fn model_written_five_page_acceptance_flow_runs_with_recorded_receipts() {
    let source = include_str!("fixtures/model-five-page-acceptance.js");
    let mut run = start_task(compile(source).unwrap(), vec![]);
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
    let run = runtime(include_str!("fixtures/quickjs-closure-callback.js"));
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
    let mut run = start(source, vec![json!({})]);
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
    let mut run = start(source, vec![json!({})]);
    assert!(matches!(run.next(), State::Action(Action { kind: ActionKind::Tool { ref args, .. }, .. }) if args["content"] == "true"));
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert!(matches!(run.next(), State::Returned(_)));
}

#[test]
fn m4_parameter_initializer_can_suspend_before_body_scope() {
    let source = "const path='outer';function read(a=io.read({path})){let path='inner';return [a.content,path];}io.write({path:'result',content:JSON.stringify(read())});";
    let mut run = start(source, vec![json!({})]);
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
    let run = runtime(include_str!("fixtures/quickjs-switch-loop.js"));
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
    let mut run = start(source, vec![Value::Null]);
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
        "throw new Other('x');",
    ] {
        assert!(
            compile(&format!(
                "/** @virtual Review and return result. */ function inspect(){{{body}}} inspect();"
            ))
            .is_err(),
            "must reject {body}"
        );
    }
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
    let program = crate::agent_program::compiler::compile_body_with_scope(
        "async function designPage(shared, page) {\n  /** @virtual */\n  async function generate(page) {\n    // Write the page. Returns {content:string}.\n  }\n  return await generate(page);\n}\n",
        "designPage", crate::agent_program::libraries::builtin(), Default::default()).unwrap();
    assert!(!program.functions["designPage"].virtual_task);
    let (id, nested) = program.functions.iter().find(|(id, _)| id.starts_with("designPage/generate@")).unwrap();
    assert!(nested.virtual_task, "{id}");
    assert!(nested.description.contains("Write the page"));
    assert_eq!(nested.leaf_tools, Some(vec![]), "declared in an implementation: a leaf");
    assert!(crate::agent_program::compiler::compile_body_with_scope(
        "function f() {\n  /** @virtual */\n  function g() {\n    // Do it.\n    return 1;\n  }\n  return g();\n}\n",
        "f", crate::agent_program::libraries::builtin(), Default::default()).is_err(), "a virtual has no executable body");
}

#[test]
fn counted_repetition_over_any_character_compiles_as_in_javascript() {
    // Written by the AI in a real run: rejected under the former 1 MB budget.
    let source = "const text = \"可核验的证据\" + \"中间\".repeat(500) + \"不能单独作为证据\";\nassert(/(?:真实的完成证据|可核验的证据)[\\s\\S]{0,1800}(?:不能单独作为证据|不能单独)/.test(text), \"found within 1800\");\nassert(!/(?:真实的完成证据|可核验的证据)[\\s\\S]{0,10}(?:不能单独作为证据|不能单独)/.test(text), \"not within 10\");";
    let mut run = Runtime::task_program(compile(source).unwrap(), crate::agent_program::ENTRY, vec![]).unwrap();
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
    let mut run = runtime(source);
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
    let mut run = runtime(source);
    let crate::agent_program::runtime::ConcurrentState::Actions(actions) = run.next_concurrent() else { panic!("expected the async call") };
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
    let mut run = runtime(source);
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
    let mut run = runtime(source);
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
    let mut run = runtime(source);
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
    let mut run = runtime("console.log('页'.repeat(99999)); console.log('x'.repeat(100001));");
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
