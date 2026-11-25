#[test]
fn binary_chains_compile_and_run_on_a_runtime_worker_stack() {
    std::thread::Builder::new().stack_size(2 * 1024 * 1024).spawn(|| {
        let count = 128;
        let terms = (0..count).map(|i| format!("tag({i})")).collect::<Vec<_>>().join(" + ");
        let source = format!("function main(){{const seen=[];function tag(n){{seen.push(n);return '<div>'+n+'</div>';}}const html='' + {terms};return [html,seen];}}");
        let expected = (0..count).map(|i| format!("<div>{i}</div>")).collect::<String>();
        assert_eq!(returned(runtime(&source)), json!([expected, (0..count).collect::<Vec<_>>() ]));
        let arithmetic = (0..count).map(|_| " - 1 + 2 * 3").collect::<String>();
        assert_eq!(returned(runtime(&format!("function main(){{return 0{arithmetic};}}"))), json!(count * 5));
        // An opt-in production capture stays outside the repository: compiling
        // it checks the actual failing input without executing its page writes.
        if let Ok(path) = std::env::var("AGENT_BINARY_CHAIN_REPRO") {
            let source = std::fs::read_to_string(path).unwrap();
            let libraries = std::collections::BTreeMap::from([("ppt_master".to_owned(),std::collections::BTreeMap::from([("apply_html".to_owned(),"async-tool:ppt_master__apply_html".to_owned())]))]);
            compile_with_libraries(&source, libraries).unwrap();
        }
    }).unwrap().join().unwrap();
}

#[test]
fn binary_chains_preserve_coercion_grouping_and_short_circuit_order() {
    let source = "function main(){const seen=[];function term(n){seen.push(n);return {valueOf(){seen.push(n+10);return n;}};}const result=term(1)+term(2)+term(3);const empty=false&&(term(4)+term(5));const right=term(6)+(term(7)+term(8));return [result,empty,right,seen,2**3**2,20-5-3];}";
    assert_eq!(returned(runtime(source)), json!([6,false,21,[1,2,11,12,3,13,6,7,8,17,18,16],512,12]));
}

#[test]
fn binary_chains_still_reject_unawaited_tool_results() {
    let source = "import * as ppt from 'ppt';const text='a'+'b'+ppt.read({});";
    let libraries = std::collections::BTreeMap::from([("ppt".to_owned(),std::collections::BTreeMap::from([("read".to_owned(),"async-tool:ppt__read".to_owned())]))]);
    let error = compile_with_libraries(source, libraries).err().expect("unawaited library call must be rejected").to_string();
    assert!(error.contains("returns a Promise"), "{error}");
}


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
    let mut top=top_level("console.log(this===undefined);");
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
fn repl_sessions_are_not_killed_by_legacy_whole_program_wall_deadline() {
    let libs = libraries::builtin();
    let mut repl = Runtime::new_repl(libs.clone()).unwrap();
    repl.append_segment("const x = 40; return x;", libs.clone(), true).unwrap();
    let State::Returned(first) = repl.next() else { panic!("expected returned value") };
    assert_eq!(first, json!(40));
    let mut snapshot = serde_json::to_value(&repl).unwrap();
    // Simulate a multi-hour agent turn (started 4 hours ago).
    snapshot["started_at_ms"] = json!(chrono::Utc::now().timestamp_millis() - 4 * 60 * 60 * 1000);
    let mut restored: Runtime = serde_json::from_value(snapshot).unwrap();
    restored.append_segment("return x + 2;", libs, true).unwrap();
    let State::Returned(second) = restored.next() else { panic!("multi-hour REPL session must not expire") };
    assert_eq!(second, json!(42));
}

#[test]
fn vm_stage0_unsupported_syntax_diagnostics_name_the_feature() {
    for (source,name) in [("new WeakRef({})","WeakRef"),("new Proxy({}, {})","Proxy")] {
        let error=compile(source).unwrap_err().to_string();
        assert!(error.contains(name),"{source}: {error}");
    }
    let url = returned(runtime("function main(){const u=new URL('https://example.com/a?x=1#h');return [u.href,u.hostname,u.search,u.hash];}"));
    assert_eq!(url, json!(["https://example.com/a?x=1#h", "example.com", "?x=1", "#h"]));
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
fn compatibility_arguments_thenables_sticky_regex_and_tagged_templates() {
    assert_eq!(
        returned(runtime(r#"function main(){function f(a){return [arguments.length,arguments[0],arguments[1]];}return f(3,4);}"#)),
        json!([2,3,4])
    );
    assert_eq!(
        returned(runtime(r#"async function main(){const thenable={then(resolve){resolve(9);}};const a=await Promise.resolve(thenable);const b=await new Promise(resolve=>resolve(thenable));return [a,b];}"#)),
        json!([9,9])
    );
    let sticky = returned(runtime(r#"function main(){const r=/a/y;r.lastIndex=1;const first=r.exec('ba');const second=r.exec('ba');return [first[0],r.lastIndex,second,r.sticky,r.unicode];}"#));
    assert_eq!(sticky, json!( ["a",0,null,true,false] ));
    assert_eq!(
        returned(runtime(r#"function main(){function tag(strings,...values){return [strings[0],strings.raw[0],values[0]];}return tag`a${3}b`; }"#)),
        json!(["a","a",3])
    );
}

#[test]
fn compatibility_class_fields_and_static_fields() {
    assert_eq!(
        returned(runtime(r#"function main(){class A{x=1;y;static z=3;m(){return this.x;}}const a=new A();return [a.x,a.y,A.z,a.m()];}"#)),
        json!([1,null,3,1])
    );
    assert_eq!(
        returned(runtime(r#"function main(){class A{#x=4;get(){return this.#x;}}return new A().get();}"#)),
        json!(4)
    );
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
fn compatibility_diagnostics_name_missing_global() {
    let error=compile("function main(){return absentGlobal(3);}").unwrap_err().to_string();
    assert!(error.contains("absentGlobal is not defined"),"{error}");
    assert!(!error.contains("call/ai wrappers"),"{error}");
    let error=compile("function main(){print('result');}").unwrap_err().to_string();
    assert!(error.contains("print is not defined") && error.contains("console.log(...)"),"{error}");
}

#[test]
fn module_loading_accepts_node_style_syntax_and_legacy_host_aliases() {
    for spec in ["io", "host:io"] {
        for binding in ["* as files", "files", "{read as load}"] {
            let call = if binding.starts_with('{') { "load" } else { "files.read" };
            let source = format!("import {binding} from '{spec}'; function main(){{return {call}({{path:'a.txt'}});}}");
            let mut run = runtime(&source);
            assert!(matches!(run.next(), State::Action(Action { kind:ActionKind::Tool { name, .. }, .. }) if name == "read_file"), "{source}");
            run.resume(json!({"content":"actual"}), false).unwrap();
            assert_eq!(returned(run), json!({"content":"actual"}));
        }
    }
    let mut run = runtime("function main(){const a=require('host:io');return a===require('io');}");
    assert!(matches!(run.next(), State::Action(Action { kind:ActionKind::Require { library }, .. }) if library == "io"));
    run.resume(json!({"library":"io","methods":{"read":"read_file"}}), false).unwrap();
    assert_eq!(returned(run), json!(true));
    assert_eq!(compiler::required_libraries("function main(){require('host:process/promises');}"), vec!["process"]);
}

#[test]
fn concrete_module_entry_accepts_its_contract_annotation() {
    let program = compile_body("/** @module Return the incremented input. */ function worker(n){return n+1;}", "worker").unwrap();
    assert!(!program.functions["worker"].virtual_task);
    assert_eq!(returned(Runtime::task_program(program, "worker", vec![json!(4)]).unwrap()), json!(5));
    let program = compile_body("/** @module Return judged input. */ function worker(n){return judge(n);}\n/** @virtual Judge n; return number. */ function judge(n){}", "worker").unwrap();
    assert!(program.functions["judge"].virtual_task);
    assert!(compile_body("/** @module Missing implementation. */ function worker(){}", "worker").is_err());
    assert!(compile("function main(){return worker();}\n/** @module Real code in a declaration. */ function worker(){return 1;}").is_err());
    assert!(compile_body("/** @module Implement entry. */ function worker(){\n/** @module A separate, nested declaration. */ function worker(){return 1;} return worker();}", "worker").is_err());
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
    assert_eq!(returned(runtime("function main(){const d=new Date(0);return [d.toLocaleString('en-US',{timeZone:'UTC'}),d.toLocaleDateString('en-US',{timeZone:'UTC'}),d.toLocaleTimeString('en-US',{timeZone:'UTC'})];}")),json!(["1/1/1970, 12:00:00 AM","1/1/1970","12:00:00 AM"]));
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
    for name in ["Object.groupBy", "Map.groupBy", "Promise.withResolvers", "RegExp.escape"] {
        assert!(js_methods::builtin_arity(name).is_some(), "{name}");
        assert!(CAPABILITIES.contains(name), "guide omits {name}");
    }
    for name in js_collections::SET_OPERATIONS.iter().copied().chain(["with", "toSpliced"]) {
        assert!(js_methods::method_arity(name).is_some(), "{name}");
        assert!(BUILTIN_CONTRACTS.contains(&format!("{name}(")) || BUILTIN_CONTRACTS.contains(&format!("{name}<")), "contract omits {name}");
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
/// Most of these tests drive one function by name, as the host drives a
/// module implementation's entry: `main` is called with the arguments and its
/// return value is the result. Nothing is guessed: JavaScript calls no function
/// by itself, and a program's own top-level statements run with `top_level`.
fn start(source: &str, args: Vec<Value>) -> Runtime {
    Runtime::new(compile(source).unwrap(), "main", args).unwrap()
}
fn start_task(program: Program, args: Vec<Value>) -> Runtime {
    Runtime::task_program(program, "main", args).unwrap()
}
fn runtime(source: &str) -> Runtime {
    start(source, vec![])
}
/// A program as the host runs one: its top-level statements, in order.
fn top_level(source: &str) -> Runtime {
    top_level_with(source, vec![])
}
fn top_level_with(source: &str, args: Vec<Value>) -> Runtime {
    Runtime::new(compile(source).unwrap(), ENTRY, args).unwrap()
}
fn top_level_task(program: Program) -> Runtime {
    Runtime::task_program(program, ENTRY, vec![]).unwrap()
}

#[test]
fn one_shot_program_supports_top_level_return_and_uncalled_checks() {
    let source = "const result = { ok: true, count: 3 }; return result;";
    let program = compile_with_libraries(source, libraries::builtin()).unwrap();
    let value = returned(top_level_task(program));
    assert_eq!(value, json!({"ok": true, "count": 3}));

    assert!(uncalled_functions("return {ok:true};", None).is_ok());
    assert!(uncalled_functions("function unused() {} return 1;", None).is_err());
}

#[test]
fn console_methods_remain_ordinary_runtime_logging_only() {
    let source = r#"console.dir({ nested: { value: 1 } }); return {done:true};"#;
    let program = compile_with_libraries(source, libraries::builtin()).unwrap();
    let mut run = top_level_task(program);
    assert_eq!(run.logs().len(), 0);
    assert!(matches!(run.next(), State::Returned(value) if value == json!({"done": true})));
    assert_eq!(run.logs().len(), 1);
    assert!(run.logs()[0].contains("nested"));
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
        r"function main(){return /x/v.test('x');}",
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
        json!([true, false])
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
fn global_regexp_exec_advances_resets_and_survives_suspension() {
    // Minimized from scratch/regex-global-repro/original-turn-2-6.js.
    let source = r#"function main() {
        const html = '<div data-kind="text" data-name="first" style="x"></div><div data-kind="shape" data-name="second" style="y"></div>';
        const re = /data-kind="([a-z]+)"\s+data-name="([^"]*)"[^>]*?style="([^"]*)"/g;
        const rows = []; let m; let iterations = 0;
        while ((m = re.exec(html)) && iterations < 5) { rows.push(m[2]); iterations++; }
        return [rows, iterations, re.lastIndex];
    }"#;
    assert_eq!(returned(runtime(source)), json!([["first", "second"], 2, 0]));
    assert_eq!(returned(runtime(r#"function main(){
        const r=/a/g; const a=r.exec('aba'); const i=r.lastIndex;
        const b=r.exec('aba'); const j=r.lastIndex; const c=r.exec('aba');
        return [a[0],i,b[0],j,c,r.lastIndex];
    }"#)), json!(["a",1,"a",3,null,0]));
    assert_eq!(returned(runtime(r#"function main(){
        const r=/a/g; r.lastIndex=2; const a=r.test('aba');
        const i=r.lastIndex; r.lastIndex=99; const b=r.test('aba');
        const plain=/a/; plain.lastIndex=2; const c=plain.test('aba');
        return [a,i,b,r.lastIndex,c,plain.lastIndex];
    }"#)), json!([true,3,false,0,true,2]));
    assert_eq!(returned(runtime(r#"function main(){const r=/a/g; r.exec('中a中a'); const i=r.lastIndex; r.exec('中a中a'); return [i,r.lastIndex];}"#)), json!([2,4]));
    assert_eq!(returned(runtime(r#"function main(){const r=/a*/g; const alias=r; r.exec('b'); const i=alias.lastIndex; alias.lastIndex=1; r.exec('b'); return [i,r.lastIndex];}"#)), json!([0,1]));
    let mut run = runtime(r#"function main(){const r=/a/g; r.exec('aba'); io.read({path:'a'}); return [r.lastIndex,r.exec('aba')[0],r.lastIndex];}"#);
    assert!(matches!(run.next(), State::Action(_)));
    let mut restored: Runtime = serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    restored.resume(json!({}), false).unwrap();
    assert_eq!(returned(restored), json!([1,"a",3]));
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
fn text_encoder_and_buffer_byte_length_count_utf8_without_a_loop() {
    let result = returned(runtime(
        r#"
function main() {
  const encoder = new TextEncoder();
  const bytes = encoder.encode("é中");
  return {
    length: bytes.length, first: bytes[0], encoding: encoder.encoding,
    inline: new TextEncoder().encode("ab").length,
    empty: new TextEncoder().encode().length,
    size: Buffer.byteLength("é中🙂"),
    utf8: Buffer.byteLength("x", "utf8"),
    kind: typeof TextEncoder,
    globalBuffer: globalThis.Buffer === Buffer,
    globalURL: globalThis.URL === URL,
  };
}
"#,
    ));
    assert_eq!(result, json!({"length":5,"first":195,"encoding":"utf-8","inline":2,"empty":0,"size":9,"utf8":1,"kind":"function","globalBuffer":true,"globalURL":true}));
    // Decoding byte arrays is unavailable; Buffer.from encodes rather than decodes.
    for source in ["function main(){return new TextDecoder().decode([104]);}"] {
        let error = compile(source).err().expect("unsupported").to_string();
        assert!(error.contains("UTF-8 decoding from byte arrays is unavailable"), "{source}: {error}");
    }
    assert_eq!(returned(runtime("function main(){return [Buffer.from('x','utf8').length, path.isAbsolute('/tmp')];}")), json!([1, true]));
    let buffer_roundtrip = returned(runtime(r#"
function main() {
  const b64 = Buffer.from("hello 中文🙂", "utf8").toString("base64");
  const decoded = Buffer.from("  " + b64.slice(0, 6) + "\n" + b64.slice(6), "base64").toString("utf8");
  const hex = Buffer.from("hello", "utf8").toString("hex");
  const fromHex = Buffer.from(hex, "hex").toString();
  const fromBytes = Buffer.from([0xe4, 0xb8, 0xad]).toString("utf8");
  const b64url = Buffer.from("++//AA==", "base64").toString("base64url");
  return {
    decoded,
    hex,
    fromHex,
    fromBytes,
    b64url,
    b64urlLen: Buffer.from(b64url, "base64url").length,
    b64ByteLen: Buffer.byteLength("aGVsbG8=", "base64"),
    hexByteLen: Buffer.byteLength("68656c6c6f", "hex"),
    isBuf: Buffer.isBuffer(Buffer.from("x")),
    isNotBuf: Buffer.isBuffer([1, 2, 3]),
  };
}
"#));
    assert_eq!(
        buffer_roundtrip,
        json!({
            "decoded": "hello 中文🙂",
            "hex": "68656c6c6f",
            "fromHex": "hello",
            "fromBytes": "中",
            "b64url": "--__AA",
            "b64urlLen": 4,
            "b64ByteLen": 5,
            "hexByteLen": 5,
            "isBuf": true,
            "isNotBuf": false
        })
    );
    let other = returned(runtime("function main(){try{return Buffer.byteLength('x','utf16le');}catch(error){return error.message;}}"));
    assert!(other.as_str().unwrap().contains("supports utf8, base64, base64url, hex, and latin1"), "{other}");
    // A local TextEncoder is the program's own.
    assert_eq!(
        returned(runtime("function main(){function TextEncoder(){this.tag='mine';} return new TextEncoder().tag;}")),
        json!("mine")
    );
}

#[test]
fn node_style_path_os_env_and_base64_resolve_to_runtime_builtins() {
    // require of a module the runtime implements needs no host link.
    let result = returned(runtime(
        r#"
function main() {
  const p = require("path");
  const os = require("node:os");
  const home = os.homedir();
  return {
    joined: p.join(home, ".dsh"),
    home: home,
    env: process.env.HOME,
    bracket: process.env["HOME"],
    missing: process.env.GROW_SURELY_UNSET_VARIABLE === undefined,
    platform: os.platform(),
    round: atob(btoa("abÿ")),
    decoded: atob(" aGk=\n"),
    kinds: [typeof atob, typeof os],
    globalHome: os.homedir() === home,
  };
}
"#,
    ));
    let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap();
    assert_eq!(result["home"], json!(home));
    assert_eq!(result["joined"], json!(std::path::Path::new(&home).join(".dsh").to_string_lossy()));
    if !cfg!(windows) {
        assert_eq!((&result["env"], &result["bracket"]), (&json!(home), &json!(home)));
    }
    assert_eq!(result["missing"], json!(true));
    assert_eq!(result["round"], json!("ab\u{ff}"));
    assert_eq!(result["decoded"], json!("hi"));
    assert_eq!(result["kinds"], json!(["function", "object"]));
    assert_eq!(result["globalHome"], json!(true));
    // Credentials stay out of flow values, and errors say what to do instead.
    let denied = returned(runtime(
        "function main(){try{return process.env.SUBAPI_API_KEY;}catch(error){return error.message;}}",
    ));
    assert!(denied.as_str().unwrap().contains("looks like a credential"), "{denied}");
    let bad = returned(runtime("function main(){try{return atob('a');}catch(error){return error.message;}}"));
    assert!(bad.as_str().unwrap().starts_with("InvalidCharacterError"), "{bad}");
    let wide = returned(runtime("function main(){try{return btoa('中');}catch(error){return error.message;}}"));
    assert!(wide.as_str().unwrap().starts_with("InvalidCharacterError"), "{wide}");
    // A source's own `process` binding is left alone.
    assert_eq!(
        returned(runtime("function main(){const process={env:{A:'local'}};return process.env.A;}")),
        json!("local")
    );
}


#[test]
fn yaml_packages_parse_and_stringify_with_the_runtime_parser() {
    let result = returned(runtime(
        r#"
function main() {
  const YAML = require("yaml");
  const jsyaml = require("js-yaml");
  const text = "base: &b\n  apiType: openai\n  timeout: 30\nproviders:\n  deepseek:\n    <<: *b\n    models: [chat, reasoner]\n  1: numeric key\nflag: yes\nempty:\n";
  const parsed = YAML.parse(text);
  return {
    parsed,
    same: JSON.stringify(jsyaml.load(text)) === JSON.stringify(parsed),
    round: YAML.parse(YAML.stringify(parsed)),
    dumped: typeof jsyaml.dump({a: [1, 2]}),
    empty: YAML.parse(""),
  };
}
"#,
    ));
    assert_eq!(result["parsed"]["providers"]["deepseek"], json!({"apiType":"openai","timeout":30,"models":["chat","reasoner"]}), "merge keys apply");
    assert_eq!(result["parsed"]["providers"]["1"], json!("numeric key"));
    assert_eq!(result["parsed"]["flag"], json!("yes"), "YAML 1.2: yes is a string");
    assert_eq!(result["parsed"]["empty"], Value::Null);
    assert_eq!(result["same"], json!(true));
    assert_eq!(result["round"], result["parsed"]);
    assert_eq!(result["dumped"], json!("string"));
    assert_eq!(result["empty"], Value::Null);
    let bad = returned(runtime("function main(){try{return require('yaml').parse('a: [1');}catch(error){return error.message;}}"));
    assert!(bad.as_str().unwrap().starts_with("YAMLParseError"), "{bad}");
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
fn regexp_arguments_become_search_query_regex_and_case_flag() {
    let mut run = runtime("function main(){return io.search({query:/foo|bar/i});}");
    let State::Action(action) = run.next() else { panic!("expected search tool action"); };
    assert!(matches!(action.kind, ActionKind::Tool { ref name, ref args } if name == "search_text" && args["query"] == "foo|bar" && args["regex"] == true && args["case_sensitive"] == false));

    let mut explicit = runtime("function main(){return io.search({query:/foo|bar/i,case_sensitive:true});}");
    let State::Action(action) = explicit.next() else { panic!("expected explicit search tool action"); };
    assert!(matches!(action.kind, ActionKind::Tool { ref name, ref args } if name == "search_text" && args["query"] == "foo|bar" && args["regex"] == true && args["case_sensitive"] == true));
}
