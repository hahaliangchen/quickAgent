use super::*;

#[test]
fn vm_stage5_basic() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{constructor(n){this.n=n;}add(x){return this.n+x;}static make(n){return new A(n);}}const a=A.make(4);return [a.add(3),a instanceof A,A.name,A.length,Object.keys(A.prototype),Object.getOwnPropertyDescriptor(A.prototype,"add").enumerable];}"#
        )),
        json!([7, true, "A", 1, [], false])
    );
}

#[test]
fn vm_stage5_accessors() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const s=Symbol("m"),log=[];class A{[log.push("key")&&s](){return this.n;}get value(){return this.n;}set value(n){this.n=n;}static get tag(){return this.name;}}const a=new A();a.value=8;return [a[s](),a.value,A.tag,log];}"#
        )),
        json!([8, 8, "A", ["key"]])
    );
}

#[test]
fn vm_stage5_call_and_method_new() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{m(){return this;}static s(){return this;}}let n=0;for(const f of [()=>A(),()=>new A.prototype.m(),()=>new A.s()])try{f();}catch(e){if(e instanceof TypeError)n++;}const m=A.prototype.m;return [n,m()===undefined,A.s()===A];}"#
        )),
        json!([3, true, true])
    );
}

#[test]
fn vm_stage5_default_derived() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{constructor(n){this.n=n;}m(){return this.n;}static s(){return 7;}}class B extends A{}const b=new B(4);return [b.m(),B.s(),b instanceof A,b instanceof B,Object.getPrototypeOf(B)===A,Object.getPrototypeOf(B.prototype)===A.prototype,B.length];}"#
        )),
        json!([4, 7, true, true, true, true, 0])
    );
}

#[test]
fn vm_stage5_super_receiver() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{m(){return this.n;}get x(){return this.n+1;}set x(v){this.n=v-1;}static m(){return this.name;}}class B extends A{constructor(){super();this.n=3;}m(){super.x=9;return [super.m(),super.x];}static m(){return super.m();}}return [new B().m(),B.m()];}"#
        )),
        json!([[8, 9], "B"])
    );
}

#[test]
fn vm_stage5_home_object() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{m(){return this.n+1;}}class B extends A{m(){return super.m();}}const obj={n:4,m:B.prototype.m};Object.setPrototypeOf(obj,{m(){return 99;}});return [obj.m(),B.prototype.m.call({n:8})];}"#
        )),
        json!([5, 9])
    );
}

#[test]
fn vm_stage5_super_arrow() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{m(){return 1;}}class B extends A{m(){return (()=>super.m()+this.n)();}}const b=new B();b.n=2;Object.setPrototypeOf(B.prototype,{m(){return 7;}});return b.m();}"#
        )),
        json!(9)
    );
}

#[test]
fn vm_stage5_derived_errors() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{}let n=0;for(const C of [class extends A{constructor(){this.x=1;}},class extends A{constructor(){}},class extends A{constructor(){return 1;}},class extends A{constructor(){super();super();}}])try{new C();}catch(e){if(e instanceof TypeError||e instanceof ReferenceError)n++;}class B extends A{constructor(){return {x:9};}}return [n,new B().x];}"#
        )),
        json!([4, 9])
    );
}

#[test]
fn vm_stage5_returns() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{constructor(){this.n=3;return 7;}}class B extends A{constructor(){super();return undefined;}}class C{constructor(){return {n:9};}}return [new B().n,new C().n];}"#
        )),
        json!([3, 9])
    );
}

#[test]
fn vm_stage5_extends_null() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A extends null{constructor(){return Object.create(new.target.prototype);}}let bad=0;for(const value of [3,{},()=>{}])try{class X extends value{}}catch(e){if(e instanceof TypeError)bad++;}let noSuper=false;try{new(class extends null{})();}catch(e){noSuper=e instanceof TypeError;}return [Object.getPrototypeOf(A.prototype)===null,new A() instanceof A,bad,noSuper];}"#
        )),
        json!([true, true, 3, true])
    );
}

#[test]
fn vm_stage5_expression_tdz() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let C=class Inner{self(){return Inner;}};const old=C;C=0;let n=0;try{class X extends X{}}catch(e){if(e instanceof ReferenceError)n++;}try{class Y{[Y](){}}}catch(e){if(e instanceof ReferenceError)n++;}return [new old().self()===old,typeof Inner,n];}"#
        )),
        json!([true, "undefined", 2])
    );
}

#[test]
fn vm_stage5_declaration_tdz() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let t=false;try{A;}catch(e){t=e instanceof ReferenceError;}class A{}const old=A;A=3;return [t,A,new old() instanceof old];}"#
        )),
        json!([true, 3, true])
    );
}

#[test]
fn vm_stage5_new_target() {
    assert_eq!(
        returned(runtime(
            r#"function main(){function A(){this.t=new.target;this.arrow=()=>new.target;}const a=new A();function f(){return new.target;}return [a.t===A,a.arrow()===A,f()===undefined];}"#
        )),
        json!([true, true, true])
    );
}

#[test]
fn vm_stage5_target_bound_reflect() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{constructor(){this.target=new.target;}}class B extends A{}const Bound=B.bind(null);function Other(){}const a=new Bound(),b=Reflect.construct(B,[],Other);return [a.target===B,b.target===Other,Object.getPrototypeOf(b)===Other.prototype,a instanceof B];}"#
        )),
        json!([true, true, true, true])
    );
}

#[test]
fn vm_stage5_super_dynamic() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{constructor(n){this.n=n;}}class B extends A{constructor(){const get=()=>this;super(7);this.same=get()===this;}}class C extends A{constructor(){super(9);}}Object.setPrototypeOf(C,B);return [new B().same,new C().n];}"#
        )),
        json!([true, 7])
    );
}

#[test]
fn vm_stage5_error() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class Custom extends Error{constructor(m){super(m);this.name="Custom";}get code(){return 7;}}try{throw new Custom("broken");}catch(e){return [e instanceof Custom,e instanceof Error,e.message,e.name,e.code,e.toString()];}}"#
        )),
        json!([true, true, "broken", "Custom", 7, "Custom: broken"])
    );
}

#[test]
fn vm_stage5_native_slots() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A extends Array{}class M extends Map{}class D extends Date{}const a=new A(2),m=new M([["a",3]]),d=new D(0);a[0]=8;return [a.length,a[0],Array.isArray(a),a instanceof A,m.get("a"),m instanceof M,d.toISOString(),d instanceof D];}"#
        )),
        json!([2, 8, true, true, 3, true, "1970-01-01T00:00:00.000Z", true])
    );
}

#[test]
fn vm_stage5_promise_boxed() {
    assert_eq!(
        returned(runtime(
            r#"async function main(){class P extends Promise{}class N extends Number{}class S extends String{}const p=new P(r=>r(8)),n=new N(4),s=new S("ok");return [p instanceof P,await p,n.valueOf(),n instanceof N,s.valueOf(),s instanceof S];}"#
        )),
        json!([true, 8, 4, true, "ok", true])
    );
}

#[test]
fn vm_stage5_descriptors() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{constructor(){}m(){}static s(){}}const p=Object.getOwnPropertyDescriptor(A,"prototype"),c=Object.getOwnPropertyDescriptor(A.prototype,"constructor"),m=Object.getOwnPropertyDescriptor(A.prototype,"m");return [p.writable,p.configurable,p.enumerable,c.value===A,c.writable,c.configurable,c.enumerable,m.writable,m.configurable,m.enumerable];}"#
        )),
        json!([
            false, false, false, true, true, true, false, true, true, false
        ])
    );
}

#[test]
fn vm_stage5_super_order() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const log=[];class A{get m(){log.push("get");return function(x){return this.n+x;};}}function key(){log.push("key");return "m";}function arg(){log.push("arg");return 3;}class B extends A{run(){return super[key()](arg());}}const b=new B();b.n=4;return [b.run(),log];}"#
        )),
        json!([7, ["key", "get", "arg"]])
    );
}

#[test]
fn vm_stage5_super_update() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{get x(){return this.n;}set x(v){this.n=v;}}class B extends A{m(){this.n=2;const old=super.x++;super.x+=3;return [old,this.n];}}return new B().m();}"#
        )),
        json!([2, 6])
    );
}

#[test]
fn vm_stage5_object_super() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const p={m(){return this.n;}},o={__proto__:p,n:7,m(){return super.m()+1;}};const m=o.m;return [o.m(),m.call({n:3})];}"#
        )),
        json!([8, 4])
    );
}

#[test]
fn vm_stage5_loop_classes() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const out=[];for(let i=0;i<3;i++){class A{m(){return i;}static self(){return A;}}out.push(A);}return [out.map(C=>new C().m()),out.every(C=>C.self()===C)];}"#
        )),
        json!([[0, 1, 2], true])
    );
}

#[test]
fn vm_stage5_derived_null_return() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{}class B extends A{constructor(){super();return null;}}try{new B();}catch(e){return e instanceof TypeError;}return false;}"#
        )),
        json!(true)
    );
}

#[test]
fn vm_stage5_method_target() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{m(){return new.target;}constructor(){this.f=function(){return new.target;};}}return [new A().m()===undefined,new A().f()===undefined];}"#
        )),
        json!([true, true])
    );
}

#[test]
fn vm_stage5_derived_snapshot() {
    let mut run = runtime(
        r#"import {read} from "host:io";function main(){class A{constructor(){this.n=read({path:"a"}).n;this.target=new.target;}}class B extends A{constructor(){const self=()=>this;super();this.same=self()===this;}m(){return this.n;}}const b=new B();return [b.m(),b.same,b.target===B,b instanceof B];}"#,
    );
    let State::Action(_) = run.next() else {
        panic!("expected host read");
    };
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":7}), false).unwrap();
    assert_eq!(returned(run), json!([7, true, true, true]));
}

#[test]
fn vm_stage5_getter_snapshot() {
    let mut run = runtime(
        r#"import {read} from "host:io";function main(){class A{get n(){return read({path:"a"}).n+this.x;}}class B extends A{m(){return super.n;}}const b=new B();b.x=2;return b.m();}"#,
    );
    let State::Action(_) = run.next() else {
        panic!("expected host read");
    };
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":7}), false).unwrap();
    assert_eq!(returned(run), json!(9));
}

#[test]
fn vm_stage5_uninitialized_snapshot() {
    let mut run = runtime(
        r#"import {read} from "host:io";function main(){class A{}class B extends A{constructor(){read({path:"a"});let bad=false;try{this;}catch(e){bad=e instanceof ReferenceError;}super();this.bad=bad;}}return new B().bad;}"#,
    );
    let State::Action(_) = run.next() else {
        panic!("expected host read");
    };
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":7}), false).unwrap();
    assert_eq!(returned(run), json!(true));
}

#[test]
fn vm_stage5_super_constructor_selected_before_args() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{constructor(){this.n="A";}}class X{constructor(){this.n="X";}}class B extends A{constructor(){super(Object.setPrototypeOf(B,X));}}return new B().n;}"#
        )),
        json!("A")
    );
}

#[test]
fn vm_stage5_anonymous_class_name() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const A=class{};let B;B=class{};return [A.name,B.name];}"#
        )),
        json!(["A", "B"])
    );
}

#[test]
fn vm_stage5_class_name_immutable() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const C=class Inner{m(){try{Inner=3;}catch(e){return e instanceof TypeError;}}};return new C().m();}"#
        )),
        json!(true)
    );
}

#[test]
fn vm_stage5_return_object_without_super_and_finally() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{}class B extends A{constructor(){try{return {n:7};}finally{}}}class C extends A{constructor(){try{return 1;}finally{return {n:9};}}}return [new B().n,new C().n];}"#
        )),
        json!([7, 9])
    );
}

#[test]
fn vm_stage5_double_super_runs_base_before_throw() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;class A{constructor(){n++;}}class B extends A{constructor(){super();try{super();}catch(e){this.bad=e instanceof ReferenceError;}}}const b=new B();return [n,b.bad];}"#
        )),
        json!([2, true])
    );
}

#[test]
fn vm_stage5_super_write_receiver_and_missing_setter() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{get x(){return 7;}}class B extends A{m(){try{super.x=9;}catch(e){return e instanceof TypeError;}}}return new B().m();}"#
        )),
        json!(true)
    );
}

#[test]
fn vm_stage5_static_super_set_and_computed() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{static set x(v){this.n=v;}static get x(){return this.n;}}class B extends A{static m(){super["x"]=7;return super["x"];}}return [B.m(),B.n,A.n===undefined];}"#
        )),
        json!([7, 7, true])
    );
}

#[test]
fn vm_stage5_native_regexp_object_and_set() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class R extends RegExp{}class O extends Object{}class S extends Set{}const r=new R("x","i"),o=new O({n:7}),s=new S([1,2,2]);return [r.test("X"),r instanceof R,o.n===undefined,o instanceof O,s.size,s instanceof S];}"#
        )),
        json!([true, true, true, true, 2, true])
    );
}

#[test]
fn vm_stage5_reflect_prototype_getter_order() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;function A(){this.target=new.target;}const T=(function(){}).bind(null);Object.defineProperty(T,"prototype",{get(){n++;return null;}});const a=Reflect.construct(A,[],T);return [n,a.target===T,Object.getPrototypeOf(a)===Object.prototype];}"#
        )),
        json!([1, true, true])
    );
}

#[test]
fn vm_stage5_default_derived_ignores_array_iterator() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{constructor(a,b){this.n=a+b;}}class B extends A{}Array.prototype[Symbol.iterator]=function(){throw new Error("no");};return new B(2,3).n;}"#
        )),
        json!(5)
    );
}

#[test]
fn vm_stage5_separate_class_evaluations() {
    assert_eq!(
        returned(runtime(
            r#"function main(){function make(n){return class{constructor(){this.n=n;}m(){return this.n;}};}const A=make(1),B=make(2);return [new A().m(),new B().m(),A!==B];}"#
        )),
        json!([1, 2, true])
    );
}

#[test]
fn vm_stage5_base_class_new_target_undefined_in_methods() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{constructor(){this.t=new.target;}m(){return new.target;}}const a=Reflect.construct(A,[],Array);return [a.t===Array,Object.getPrototypeOf(a)===Array.prototype,A.prototype.m.call(a)===undefined];}"#
        )),
        json!([true, true, true])
    );
}

#[test]
fn vm_stage5_heritage_getter_snapshot() {
    let mut run = runtime(
        r#"import {read} from "host:io";function main(){const Parent=(function(){}).bind(null);Object.defineProperty(Parent,"prototype",{get(){return {n:read({path:"a"}).n};}});class B extends Parent{}const b=new B();return [b.n,b instanceof B];}"#,
    );
    let State::Action(_) = run.next() else {
        panic!("expected host read");
    };
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":7}), false).unwrap();
    assert_eq!(returned(run), json!([7, true]));
}

#[test]
fn vm_stage5_reflect_target_getter_snapshot() {
    let mut run = runtime(
        r#"import {read} from "host:io";function main(){function A(){this.target=new.target;}const T=A.bind(null);Object.defineProperty(T,"prototype",{get(){return {n:read({path:"a"}).n};}});const a=Reflect.construct(A,[],T);return [a.n,a.target===T];}"#,
    );
    let State::Action(_) = run.next() else {
        panic!("expected host read");
    };
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":7}), false).unwrap();
    assert_eq!(returned(run), json!([7, true]));
}

#[test]
fn vm_stage5_gc_extracted_home_snapshot() {
    let mut run = runtime(
        r#"import {read} from "host:io";function main(){function make(){class A{m(){return 3;}}class B extends A{m(){return super.m()+read({path:"a"}).n;}}return B.prototype.m;}const f=make();for(let i=0;i<400;i++){const garbage=[i];}return f.call({});}"#,
    );
    let State::Action(_) = run.next() else {
        panic!("expected host read");
    };
    run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
    run.resume(json!({"n":7}), false).unwrap();
    assert_eq!(returned(run), json!(10));
}

#[test]
fn vm_stage5_strict_restricted_properties() {
    assert_eq!(
        returned(runtime(
            r#"function main(){class A{m(){}}let n=0;for(const f of [A,A.prototype.m])for(const k of ['caller','arguments'])try{f[k];}catch(e){if(e instanceof TypeError)n++;}return n;}"#
        )),
        json!(4)
    );
}
#[test]
fn vm_stage5_super_key_conversion_after_base() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const first={p:7},second={p:9};const o={__proto__:first,m(k){return super[k];}},key={toString(){Object.setPrototypeOf(o,second);return 'p';}};return o.m(key);}"#
        )),
        json!(7)
    );
}
#[test]
fn vm_stage5_super_key_not_evaluated_before_this() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;class A{}class B extends A{constructor(){super[super()];}}try{new B();}catch(e){if(e instanceof ReferenceError)n++;}return n;}"#
        )),
        json!(1)
    );
}

#[test]
fn vm_stage5_boxed_constructor_conversion() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const o={valueOf(){n++;return 7;},toString(){n++;return 'ok';}};class N extends Number{}class S extends String{}let bad=false;try{new S(Symbol());}catch(e){bad=e instanceof TypeError;}return [new N(o).valueOf(),new S(o).valueOf(),n,bad];}"#
        )),
        json!([7, "ok", 2, true])
    );
}
#[test]
fn vm_stage5_error_location() {
    let mut run =
        runtime("function main(){\nclass Custom extends Error{}\nthrow new Custom('broken');\n}");
    let State::Failed(e) = run.next() else {
        panic!("expected Custom error");
    };
    assert_eq!(e["message"], json!("broken"));
    assert_eq!(e["location"]["line"], json!(3));
}

#[test]
fn vm_stage5_super_assignment_converts_key_after_rhs() {
    assert_eq!(
        returned(runtime(
            r#"function main(){const log=[],key={toString(){log.push('key');return 'x';}},o={__proto__:{},m(){super[key]=rhs();}};function rhs(){log.push('rhs');return 7;}o.m();return [log,o.x];}"#
        )),
        json!([["rhs", "key"], 7])
    );
}
#[test]
fn vm_stage5_super_update_key_conversion_twice() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const key={toString(){n++;return 'x';}},o={__proto__:{x:1},m(){return super[key]++;}};return [o.m(),o.x,n];}"#
        )),
        json!([1, 2, 2])
    );
}
#[test]
fn vm_stage5_null_super_base_does_not_convert_key() {
    assert_eq!(
        returned(runtime(
            r#"function main(){let n=0;const key={toString(){n++;return 'x';}},o={__proto__:null,m(){return super[key];}};let bad=false;try{o.m();}catch(e){bad=e instanceof TypeError;}return [bad,n];}"#
        )),
        json!([true, 0])
    );
}
