use super::*;

#[test]
fn vm_stage3c_host_boundary_does_not_silently_drop_accessors() {
    let mut run = runtime(r#"function main(){return {get x(){return 7;}};}"#);
    assert!(
        matches!(run.next(),State::Failed(e) if e["message"].as_str().is_some_and(|m|m.contains("accessor")&&m.contains("JSON.stringify")))
    );
}

#[test]
fn vm_stage3c_reflect_key_conversion_executes_user_code_and_throws() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const key={toString(){n++;throw new RangeError('key');}},o={};for(const f of [()=>Reflect.has(o,key),()=>Reflect.deleteProperty(o,key),()=>Reflect.getOwnPropertyDescriptor(o,key),()=>Object.getOwnPropertyDescriptor(o,key)]){try{f();}catch(e){if(!(e instanceof RangeError))throw e;}}return n;}"#
        )),
        json!(4)
    );
}

#[test]
fn vm_stage3c_bind_reads_metadata_accessors_and_bound_reflect_construct() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const log=[];function C(a){this.n=a;}Object.defineProperty(C,'length',{get(){log.push('length');return Infinity;}});Object.defineProperty(C,'name',{get(){log.push('name');return 'Custom';}});const B=C.bind(null,7),o=Reflect.construct(B,[]);return [B.length===Infinity,B.name,log,o.n,o instanceof C];}"#
        )),
        json!([true, "bound Custom", ["length", "name"], 7, true])
    );
}

#[test]
fn vm_stage3c_function_own_key_order_and_deleted_metadata() {
    assert_eq!(
        returned(runtime(
            r#"function main(){function f(a){}const before=Reflect.ownKeys(f);delete f.length;Object.defineProperty(f,'length',{value:4});return [before,Reflect.ownKeys(f)];}"#
        )),
        json!([
            ["length", "name", "prototype"],
            ["name", "prototype", "length"]
        ])
    );
}

#[test]
fn vm_stage3c_array_iteration_and_array_from_execute_accessors() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const a=[0];Object.defineProperty(a,'0',{get(){n++;return 7;}});const out=[];for(const v of a)out.push(v);const [x]=a;const o={get length(){n++;return 1;},get 0(){n++;return 9;}};return [out,x,Array.from(a),Array.from(o),n];}"#
        )),
        json!([[7], 7, [7], [9], 5])
    );
}

#[test]
fn vm_stage3c_descriptor_records_ignore_object_prototype_poisoning() {
    assert_eq!(
        returned(runtime(
            r#"function main(){Object.defineProperty(Object.prototype,'value',{get(){return 9;},configurable:true});const o={};Object.defineProperty(o,'x',{value:3});delete Object.prototype.value;return o.x;}"#
        )),
        json!(3)
    );
}
#[test]
fn vm_stage3c_instanceof_reads_prototype_getter_once() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;Object.defineProperty(Function.prototype,'prototype',{get(){n++;return Array.prototype;},configurable:true});return [[] instanceof Function.prototype,1 instanceof Function.prototype,n];}"#
        )),
        json!([true, false, 1])
    );
}
#[test]
fn vm_stage3c_computed_key_coercion_precedes_value() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const key={toString(){n++;return 'x';}},o={[key]:n};return [o.x,n];}"#
        )),
        json!([1, 1])
    );
}
#[test]
fn vm_stage3c_boxed_string_descriptors_use_utf16() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const s=Object('😀');return [Object.getOwnPropertyNames(s),Object.getOwnPropertyDescriptor(s,'length'),Reflect.defineProperty(s,'0',{value:'x'}),s.length];}"#
        )),
        json!([["0","1","length"],{"value":2,"writable":false,"enumerable":false,"configurable":false},false,2])
    );
}

#[test]
fn vm_stage3c_array_mutators_obey_descriptors_and_partial_changes() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const a=[1,2,3];Object.defineProperty(a,'1',{writable:false});let bad=0;try{a.fill(9);}catch(e){if(e instanceof TypeError)bad++;}const b=[1];Object.preventExtensions(b);try{b.push(2);}catch(e){if(e instanceof TypeError)bad++;}const c=[1,2];Object.defineProperty(c,'length',{writable:false});try{c.pop();}catch(e){if(e instanceof TypeError)bad++;}const d=[2,1];Object.defineProperty(d,'0',{writable:false});try{d.sort();}catch(e){if(e instanceof TypeError)bad++;}return [bad,a,b,c.length,Object.hasOwn(c,'1')];}"#
        )),
        json!([4, [9, 2, 3], [1], 2, false])
    );
}
#[test]
fn vm_stage3c_reflect_apply_reads_list_accessors_once() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const log=[],a={get length(){log.push('length');return 2;},get 0(){log.push('0');return 3;},get 1(){log.push('1');return 4;}};function f(x,y){return this.n+x+y;}f.apply=()=>0;return [Reflect.apply(f,{n:1},a),log];}"#
        )),
        json!([8, ["length", "0", "1"]])
    );
}
#[test]
fn vm_stage3c_explicit_undefined_receiver_is_preserved() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const o={get x(){return this===undefined;},set x(v){if(this!==undefined)throw new Error('receiver');}};return [Reflect.get(o,'x',undefined),Reflect.set(o,'x',1,undefined)];}"#
        )),
        json!([true, true])
    );
}
#[test]
fn vm_stage3c_empty_object_pattern_does_not_read_empty_key() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const o={get ''(){n++;return 1;}};const {}=o;return n;}"#
        )),
        json!(0)
    );
}
#[test]
fn vm_stage3c_public_method_lengths_and_reflect_namespace() {
    assert_eq!(
        returned(runtime(
            r#"function main(){return [typeof Reflect,Object.defineProperty.length,Object.defineProperties.length,Object.getOwnPropertyDescriptor.length,Reflect.get.length,Reflect.set.length,Reflect.apply.length,Reflect.construct.length];}"#
        )),
        json!(["object", 3, 2, 2, 2, 3, 3, 2])
    );
}

#[test]
fn vm_stage3c_descriptor_defaults_and_enumeration() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const o={};Object.defineProperty(o,'x',{value:7});const d=Object.getOwnPropertyDescriptor(o,'x');return [o.x,d,Object.keys(o),Object.getOwnPropertyNames(o),Reflect.ownKeys(o)];}"#
        )),
        json!([7,{"value":7,"writable":false,"enumerable":false,"configurable":false},[],["x"],["x"]])
    );
}
#[test]
fn vm_stage3c_nonconfigurable_and_same_value() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const o={};Object.defineProperty(o,'x',{value:NaN});Object.defineProperty(o,'x',{value:NaN});let bad=0;for(const d of [{value:2},{writable:true},{enumerable:true},{configurable:true},{get(){return 1;}}]){try{Object.defineProperty(o,'x',d);}catch(e){if(e instanceof TypeError)bad++;}}const z={};Object.defineProperty(z,'x',{value:0});return [bad,Reflect.defineProperty(z,'x',{value:-0}),Reflect.deleteProperty(o,'x'),Number.isNaN(o.x)];}"#
        )),
        json!([5, false, false, true])
    );
}
#[test]
fn vm_stage3c_data_redefinition_and_deletion() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const o={x:1};Object.defineProperty(o,'x',{enumerable:false});o.x=2;const a=Object.getOwnPropertyDescriptor(o,'x');delete o.x;o.x=3;return [a,Object.getOwnPropertyDescriptor(o,'x'),Object.keys(o)];}"#
        )),
        json!([{"value":2,"writable":true,"enumerable":false,"configurable":true},{"value":3,"writable":true,"enumerable":true,"configurable":true},["x"]])
    );
}
#[test]
fn vm_stage3c_literal_accessors_and_computed_names() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=2;const key='x',o={get [key](){return n;},set [key](v){n=v;}};o.x+=3;const d=Object.getOwnPropertyDescriptor(o,key);return [o.x,typeof d.get,typeof d.set,d.enumerable,d.configurable,'value' in d,d.get.name,d.set.name];}"#
        )),
        json!([
            5, "function", "function", true, true, false, "get x", "set x"
        ])
    );
}
#[test]
fn vm_stage3c_inherited_accessor_receiver_and_updates() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const p={get x(){return this.n;},set x(v){this.n=v;}},o=Object.create(p);o.n=2;const before=o.x++;o.x*=3;return [before,o.x,Object.hasOwn(o,'x'),Reflect.get(p,'x',{n:9}),Reflect.set(p,'x',7,o),o.n];}"#
        )),
        json!([2, 9, false, 9, true, 7])
    );
}
#[test]
fn vm_stage3c_missing_setter_and_inherited_readonly() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const p={get x(){return 1;}};Object.defineProperty(p,'y',{value:2});const o=Object.create(p);let bad=0;try{o.x=3;}catch(e){if(e instanceof TypeError)bad++;}try{o.y=4;}catch(e){if(e instanceof TypeError)bad++;}return [bad,Reflect.set(o,'x',3),Reflect.set(o,'y',4),Object.hasOwn(o,'x'),o.y];}"#
        )),
        json!([2, false, false, false, 2])
    );
}
#[test]
fn vm_stage3c_descriptor_conversion_and_invalid_mixed_fields() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const log=[],d=Object.create({get enumerable(){log.push('e');return true;},get configurable(){log.push('c');return true;},get value(){log.push('v');return 7;},get writable(){log.push('w');return true;}}),o={};Object.defineProperty(o,'x',d);let bad=0;for(const desc of [{get:3},{set:null},{value:1,get:undefined},{writable:false,set:undefined}]){try{Reflect.defineProperty(o,'z',desc);}catch(e){if(e instanceof TypeError)bad++;}}return [o.x,log,bad];}"#
        )),
        json!([7, ["e", "c", "v", "w"], 4])
    );
}
#[test]
fn vm_stage3c_define_properties_converts_before_changes() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const o={};let bad=false;try{Object.defineProperties(o,{a:{value:1},b:{get:3}});}catch(e){bad=e instanceof TypeError;}Object.defineProperties(o,{x:{value:7,enumerable:true}});const p=Object.create(null,{y:{value:8}});return [bad,'a' in o,o.x,p.y,Object.getPrototypeOf(p)===null];}"#
        )),
        json!([true, false, 7, 8, true])
    );
}
#[test]
fn vm_stage3c_integrity_levels_and_accessor_freeze() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=1;const o={get x(){return n;},set x(v){n=v;}};Object.freeze(o);o.x=4;const p=Object.seal({a:1});p.a=2;const q=Object.preventExtensions({a:1});delete q.a;return [o.x,Object.isFrozen(o),Object.isSealed(o),Object.isExtensible(o),p.a,Reflect.deleteProperty(p,'a'),Reflect.set(p,'z',3),Object.isSealed(q),Object.isFrozen(q),Object.isExtensible(3),Object.isFrozen(3)];}"#
        )),
        json!([
            4, true, true, false, 2, false, false, true, true, false, true
        ])
    );
}
#[test]
fn vm_stage3c_reflect_target_validation_and_prototypes() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let bad=0;for(const f of [()=>Reflect.get(1,'x'),()=>Reflect.set(null,'x',1),()=>Reflect.has('x','length'),()=>Reflect.ownKeys(1),()=>Reflect.isExtensible(1),()=>Reflect.preventExtensions(1),()=>Reflect.getPrototypeOf(1),()=>Reflect.setPrototypeOf(1,null),()=>Reflect.deleteProperty(1,'x')]){try{f();}catch(e){if(e instanceof TypeError)bad++;}}const o={},p={};return [bad,Reflect.setPrototypeOf(o,p),Reflect.getPrototypeOf(o)===p,Reflect.setPrototypeOf(p,o),Reflect.preventExtensions(o),Reflect.isExtensible(o),Reflect.setPrototypeOf(o,null)];}"#
        )),
        json!([9, true, true, false, true, false, false])
    );
}
#[test]
fn vm_stage3c_reflect_set_distinct_receiver() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const target={x:1},r={};const a=Reflect.set(target,'x',7,r);Object.defineProperty(r,'z',{get(){return 2;}});return [a,target.x,r.x,Reflect.set(target,'z',3,r),Reflect.set(target,'x',3,1),Reflect.has(r,'x'),Reflect.getOwnPropertyDescriptor(r,'x')];}"#
        )),
        json!([true,1,7,false,false,true,{"value":7,"writable":true,"enumerable":true,"configurable":true}])
    );
}
#[test]
fn vm_stage3c_reflect_apply_and_construct() {
    assert_eq!(
        returned(runtime(
            r#"function main(){function C(a){this.n=a;}function D(){}const a=Reflect.construct(C,[7],D);return [Reflect.apply(function(a,b){return this.n+a+b;},{n:1},{0:2,1:3,length:2}),a.n,a instanceof D,a instanceof C];}"#
        )),
        json!([6, 7, true, false])
    );
}
#[test]
fn vm_stage3c_getter_throws_and_short_circuit_order() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const log=[],o={get x(){log.push('get');throw new RangeError('x');}};let bad=false;try{o.x;}catch(e){bad=e instanceof RangeError;}false&&o.x;true||o.x;const n=null;n?.x;return [bad,log];}"#
        )),
        json!([true, ["get"]])
    );
}
#[test]
fn vm_stage3c_getter_returned_method_before_arguments() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const log=[],o={n:7,get f(){log.push('get');return function(x){return this.n+x;};}};function arg(){log.push('arg');return 2;}return [o.f(arg()),log];}"#
        )),
        json!([9, ["get", "arg"]])
    );
}
#[test]
fn vm_stage3c_accessors_survive_host_suspend_and_gc() {
    let mut run = runtime(
        r#"function main(){let n=1;const o={get x(){const r=io.read({});return r.n+n;},set x(v){io.read({});n=v;}};for(let i=0;i<400;i++){const a=[i];}o.x=3;return o.x;}"#,
    );
    assert!(matches!(run.next(), State::Action(_)));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({}), false).unwrap();
    assert!(matches!(run.next(), State::Action(_)));
    let mut run: Runtime = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":4}), false).unwrap();
    assert_eq!(returned(run), json!(7));
}
#[test]
fn vm_stage3c_array_descriptors_and_length_failure() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const a=[1,2,3];Object.defineProperty(a,'1',{configurable:false});const shrink=Reflect.defineProperty(a,'length',{value:0,writable:false});return [shrink,a.length,Object.getOwnPropertyDescriptor(a,'length'),Reflect.set(a,'2',9),Reflect.deleteProperty(a,'1'),a];}"#
        )),
        json!([false,2,{"value":2,"writable":false,"enumerable":false,"configurable":false},false,false,[1,2]])
    );
}
#[test]
fn vm_stage3c_array_accessor_and_readonly_index() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const a=[1,2];Object.defineProperty(a,'0',{get(){return 7;},enumerable:true,configurable:true});Object.defineProperty(a,'1',{writable:false});let bad=false;try{a[1]=9;}catch(e){bad=e instanceof TypeError;}return [a[0],a.map(v=>v*2),bad,Object.keys(a)];}"#
        )),
        json!([7, [14, 4], true, ["0", "1"]])
    );
}
#[test]
fn vm_stage3c_object_values_assign_spread_and_destructure_getters() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let count=0;const o={get x(){count++;return 7;}};const values=Object.values(o),entries=Object.entries(o),copy=Object.assign({},o),spread={...o}, {x}=o;return [values,entries,copy,spread,x,count];}"#
        )),
        json!([[7],[["x",7]],{"x":7},{"x":7},7,5])
    );
}
#[test]
fn vm_stage3c_json_stringify_calls_getters_and_ignores_non_enumerable() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const o={get x(){n++;return 7;}};Object.defineProperty(o,'hidden',{get(){throw new Error('hidden');}});return [JSON.stringify(o),n];}"#
        )),
        json!(["{\"x\":7}", 1])
    );
}
#[test]
fn vm_stage3c_builtin_descriptors_and_function_metadata_delete() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const c=Object.getOwnPropertyDescriptor(Array,'prototype'),m=Object.getOwnPropertyDescriptor(Array.prototype,'map'),g=Object.getOwnPropertyDescriptor(Map.prototype,'size');function f(a){}const before=Object.getOwnPropertyDescriptor(f,'length');delete f.length;Object.defineProperty(f,'name',{value:'renamed'});return [c.writable,c.enumerable,c.configurable,m.writable,m.enumerable,m.configurable,typeof g.get,g.set===undefined,before,f.length,f.name];}"#
        )),
        json!([false,false,false,true,false,true,"function",true,{"value":1,"writable":false,"enumerable":false,"configurable":true},0,"renamed"])
    );
}
