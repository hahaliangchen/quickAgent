use super::*;

macro_rules! js_test {
    ($name:ident, $src:literal, $expected:expr) => {
        #[test]
        fn $name() {
            assert_eq!(returned(runtime($src)), $expected);
        }
    };
}

js_test!(
    vm_stage8_sequence,
    "function main(){let a=0,b=0,n=0;a=1,b=2;for(let i=0,j=9;i<j;i++,j--)n++;return [(1,2),a,b,n];}",
    json!([2, 1, 2, 5])
);
js_test!(
    vm_stage8_destructure_assignment,
    "function main(){let a=1,b=2;const v=[b,a],o={};const result=([a,b]=v);({a}={a:7});[o.x]=[3];[b=5]=[];[...o.r]=[1,2];return [a,b,o,result===v];}",
    json!([7,5,{"x":3,"r":[1,2]},true])
);
js_test!(
    vm_stage8_destructure_order,
    "function main(){const log=[],o={},v={get a(){log.push('get');return undefined;}};function k(){log.push('key');return 'x';}function d(){log.push('default');return 5;}({a:o[k()]=d()}=v);return [log,o];}",
    json!([["key","get","default"],{"x":5}])
);
js_test!(
    vm_stage8_destructure_iterator_order,
    "function main(){const log=[],o={},v={[Symbol.iterator](){return {next(){log.push('next');return {done:true};}};}};function k(){log.push('key');return 'x';}[o[k()]=7]=v;[...o[k()]]=v;return [log,o];}",
    json!([["key","next","key","next"],{"x":[]}])
);
js_test!(
    vm_stage8_destructure_loop_super,
    "function main(){let a,b;for([a,b] of [[1,2],[3,4]]){}class A{set x(v){this.y=v;}}class B extends A{m(){[super.x]=[7];}}const o=new B();o.m();return [a,b,o.y];}",
    json!([3, 4, 7])
);
#[test]
fn vm_stage8_destructure_const() {
    for src in [
        "function main(){const a=0;[a]=[1];}",
        "function main(){const a=0;({a}={a:1});}",
    ] {
        assert!(compile(src).is_err(), "{src}");
    }
}
js_test!(
    vm_stage8_array_holes,
    "function main(){const a=[1,,3],b=[1,,],c=[,...[1]],d=[,,];delete a[0];return [a.length,1 in a,b.length,1 in b,c.length,0 in c,c[1],d.length,0 in d,a.length];}",
    json!([3, false, 2, false, 2, false, 1, 2, false, 3])
);
js_test!(
    vm_stage8_constructor_spread,
    "async function main(){return [new Date(...[0]).getTime(),new Map(...[[[1,2]]]).get(1),new Error(...['x']).message,new Set(...[[1,2]]).size,new RegExp(...['a','i']).test('A'),await new Promise(...[r=>r(7)])];}",
    json!([0, 2, "x", 2, true, 7])
);

js_test!(
    vm_stage8_default_derived_fields,
    "function main(){class A{a=1}class B extends A{b=2}return [Object.keys(new B()),new B()];}",
    json!([["a","b"],{"a":1,"b":2}])
);
js_test!(
    vm_stage8_private_reflection,
    "function main(){class A{a=1;#b=2;read(){return this.#b;}}const o=new A();Object.freeze(o);return [Object.keys(o),Object.getOwnPropertyNames(o),Reflect.ownKeys(o),JSON.stringify(o),{...o},Object.assign({},o),o.read()];}",
    json!([["a"],["a"],["a"],"{\"a\":1}",{"a":1},{"a":1},2])
);
js_test!(
    vm_stage8_private_brand,
    "function main(){class A{#x=1;static read(o){return o.#x;}static write(o){o.#x=2;}}const a=new A(),b=Object.create(a);let n=0;for(const o of [{},b]){try{A.read(o);}catch(e){if(e instanceof TypeError)n++;}try{A.write(o);}catch(e){if(e instanceof TypeError)n++;}}return [n,A.read(a)];}",
    json!([4, 1])
);
js_test!(
    vm_stage8_field_scope,
    "function main(){let x=3;class A{a=x;constructor(x=9){}}return new A(7).a;}",
    json!(3)
);
js_test!(
    vm_stage8_field_before_parameters,
    "function main(){const log=[];class A{x=log.push('field');constructor(p=log.push('param')){log.push('body');}}new A();return log;}",
    json!(["field", "param", "body"])
);
js_test!(
    vm_stage8_field_define,
    "function main(){let n=0;class A{set x(v){n++;}}class B extends A{x=7;}const b=new B();return [n,b.x,Object.getOwnPropertyDescriptor(b,'x')];}",
    json!([0,7,{"value":7,"writable":true,"enumerable":true,"configurable":true}])
);
js_test!(
    vm_stage8_field_computed_once,
    "function main(){let n=0;const log=[];class A{[(log.push('a'),++n)]=1;[(log.push('b'),++n)](){return 2;}static x=log.push('static');}const a=new A(),b=new A();return [n,log,Object.keys(a),Object.keys(b),a[2]()];}",
    json!([2, ["a", "b", "static"], ["1"], ["1"], 2])
);
js_test!(
    vm_stage8_field_names,
    "function main(){class A{x=function(){};['y']=()=>0;#z=function(){};read(){return this.#z.name;}}const a=new A();return [a.x.name,a.y.name,a.read()];}",
    json!(["x", "y", "#z"])
);
js_test!(
    vm_stage8_conditional_super_fields,
    "function main(){class A{}class B extends A{x=7;constructor(x){if(x)super();else (()=>super())();}}return [new B(true).x,new B(false).x];}",
    json!([7, 7])
);
js_test!(
    vm_stage8_static_elements,
    "function main(){const log=[];class A{static x=2;static y=A.x+1;static z=this.y+1;static{var v=7;this.x=v;log.push(this===A);}static w=typeof v;static{var v=8;log.push(v);}}return [A.x,A.y,A.z,A.w,log];}",
    json!([7, 3, 4, "undefined", [true, 8]])
);
js_test!(
    vm_stage8_private_methods,
    "function main(){class A{#x=2;#m(){return this.#x;}get #v(){return this.#m();}set #v(v){this.#x=v;}static #s(){return 9;}c(){this.#v=7;return [this.#v,this.#m.name,A.#s()];}}return new A().c();}",
    json!([7, "#m", 9])
);
js_test!(
    vm_stage8_private_in,
    "function main(){class A{#x;#m(){}static #y;static h(o){return [#x in o,#m in o,#y in o];}}let bad=false;try{A.h(1);}catch(e){bad=e instanceof TypeError;}return [A.h(new A()),A.h(A),A.h({}),bad];}",
    json!([
        [true, true, false],
        [false, false, true],
        [false, false, false],
        true
    ])
);
js_test!(
    vm_stage8_private_factory_identity,
    "function main(){function make(){return class{#x=1;static read(o){return o.#x;}};}const A=make(),B=make();let bad=false;try{A.read(new B());}catch(e){bad=e instanceof TypeError;}return [A.read(new A()),B.read(new B()),bad];}",
    json!([1, 1, true])
);
js_test!(
    vm_stage8_private_frozen,
    "function main(){let object;class A{constructor(){object=Object.freeze({});return object;}}class B extends A{#x=1;static set(o){o.#x++;return o.#x;}}const b=new B();let duplicate=false;try{new B();}catch(e){duplicate=e instanceof TypeError;}return [B.set(b),Reflect.ownKeys(b),Object.isFrozen(b),duplicate];}",
    json!([2, [], true, false])
);
js_test!(
    vm_stage8_private_reinitialize,
    "function main(){const o=Object.preventExtensions({});class A{constructor(){return o;}}class B extends A{#x=1;}new B();try{new B();}catch(e){return e instanceof TypeError;}return false;}",
    json!(true)
);
js_test!(
    vm_stage8_private_readonly,
    "function main(){class A{#m(){}get #g(){return 1;}set #s(x){}check(){let n=0;try{this.#m=1;}catch(e){if(e instanceof TypeError)n++;}try{this.#g=1;}catch(e){if(e instanceof TypeError)n++;}try{return this.#s;}catch(e){if(e instanceof TypeError)n++;}return n;}}return new A().check();}",
    json!(3)
);
js_test!(
    vm_stage8_private_updates_destructure_optional,
    "function main(){class A{#x=1;static f(o){o.#x+=2;const n=o.#x++;[o.#x]=[7];return [n,o?.#x,null?.#x];}}return A.f(new A());}",
    json!([3, 7, null])
);
js_test!(
    vm_stage8_private_async_generators,
    "async function main(){class A{async #a(){return 7;}*#g(){yield 8;}static async *#s(){yield 9;}async run(){return [await this.#a(),this.#g().next().value,(await A.#s().next()).value];}}return await new A().run();}",
    json!([7, 8, 9])
);
#[test]
fn vm_stage8_private_undeclared() {
    for src in [
        "function main(){class A{m(){return this.#missing;}}}",
        "function main(){class A{#x;}return {}.#x;}",
    ] {
        assert!(compile(src).is_err());
    }
}

js_test!(
    vm_stage8_regex_lookaround,
    r#"function main(){return [/a(?=b)/.test('ab'),/a(?!b)/.test('ac'),/(?<=a)b/.exec('ab')[0],/(?<!a)b/.test('cb'),/\bfoo(?=bar)/.test('foobar')];}"#,
    json!([true, true, "b", true, true])
);
js_test!(
    vm_stage8_regex_backrefs,
    r#"function main(){return [/(a)\1/.test('aa'),/(?<x>a)\k<x>/.test('aa'),/(?<x>a)\k<x>/.exec('aa').groups.x];}"#,
    json!([true, true, "a"])
);
js_test!(
    vm_stage8_regex_unicode_properties,
    r#"function main(){return [/\p{L}/u.test('中'),/\p{Script=Han}/u.test('中'),/\P{L}/u.test('1'),/\p{L}/.test('p{L}')];}"#,
    json!([true, true, true, true])
);
js_test!(
    vm_stage8_regex_consumers,
    r#"function main(){const r=/a(?=b)/gy;const a=r.exec('ab'),n=r.lastIndex,b=r.exec('ab');return [a[0],n,b,r.lastIndex,Array.from('abab'.matchAll(/a(?=b)/g),m=>m.index),'abab'.replace(/a(?=b)/g,'x'),'abac'.split(/a(?=b)/),'aa'.match(/(a)\1/g),'ab'.search(/(?<=a)b/)];}"#,
    json!(["a", 1, null, 0, [0, 2], "xbxb", ["", "bac"], ["aa"], 1])
);
js_test!(
    vm_stage8_regex_backtrack_error,
    r#"function main(){try{return /^(a|aa)+\1$/.test('a'.repeat(64)+'!');}catch(e){return e.message.includes('backtrack');}}"#,
    json!(true)
);

js_test!(
    vm_stage8_async_array_callbacks,
    "async function main(){const log=[];const v=[1,2].forEach(async x=>{log.push(x);await 0;log.push(x+10);});const before=log.slice();const filtered=[1,2].filter(async()=>false);const some=[1].some(async()=>false),every=[1].every(async()=>false);async function f(){return false;}const named=[1].filter(f);await 0;return [v,before,filtered,some,every,named,log];}",
    json!([null, [1, 2], [1, 2], true, true, [1], [1, 2, 11, 12]])
);
js_test!(
    vm_stage8_async_reduce_collections,
    "async function main(){const p=[1,2,3].reduce(async(a,x)=>(await a)+x,Promise.resolve(0));const log=[];new Set([1]).forEach(async x=>{log.push(x);await 0;});new Map([[2,3]]).forEach(async(v,k)=>log.push(k+v));return [p instanceof Promise,await p,log];}",
    json!([true, 6, [1, 5]])
);
js_test!(
    vm_stage8_generator_callbacks,
    "function main(){function* g(){yield 1;}return [[1].filter(g),[1].map(g)[0].next().value];}",
    json!([[1], 1])
);
js_test!(
    vm_stage8_object_is,
    "function main(){const o={};return [Object.is(NaN,NaN),Object.is(0,-0),Object.is(-0,-0),Object.is(o,o),Object.is({},{}),Object.is.length];}",
    json!([true, false, true, true, false, 2])
);
js_test!(
    vm_stage8_uri,
    "function main(){const s='https://例子.cn/a b?x=1&y=#';return [encodeURI(s),decodeURI(encodeURI(s)),decodeURI('%2f%3F%23%41'),encodeURIComponent(';/?:@&=+$,#')];}",
    json!([
        "https://%E4%BE%8B%E5%AD%90.cn/a%20b?x=1&y=#",
        "https://例子.cn/a b?x=1&y=#",
        "%2f%3F%23A",
        "%3B%2F%3F%3A%40%26%3D%2B%24%2C%23"
    ])
);
js_test!(
    vm_stage8_uri_errors,
    r#"function main(){let n=0;for(const s of ['%','%FF','%E0%80%80','%ED%A0%80']){try{decodeURI(s);}catch(e){if(e instanceof URIError)n++;}}try{encodeURI('\uD800');}catch(e){if(e instanceof URIError)n++;}return n;}"#,
    json!(5)
);
js_test!(
    vm_stage8_locale_compare,
    "function main(){return ['2'.localeCompare('10','en',{numeric:true}),'a'.localeCompare('A','en',{sensitivity:'base'}),'a'.localeCompare('A','en',{caseFirst:'upper'}),'阿'.localeCompare('波','zh'),'é'.localeCompare('e','en',{sensitivity:'accent'})];}",
    json!([-1, 0, 1, -1, 1])
);
js_test!(
    vm_stage8_locale_numbers,
    "function main(){return [(1234.5).toLocaleString('en-US'),(1234.5).toLocaleString('zh-CN'),Number.prototype.toLocaleString.call(12)];}",
    json!(["1,234.5", "1,234.5", "12"])
);
js_test!(
    vm_stage8_locale_dates,
    "function main(){const d=new Date(0);return [d.toLocaleString('en-US',{timeZone:'UTC'}),d.toLocaleDateString('en-US',{timeZone:'UTC'}),d.toLocaleTimeString('en-US',{timeZone:'UTC'}),d.toLocaleString('zh-CN',{timeZone:'UTC'}),d.toLocaleTimeString('en-US',{timeZone:'UTC',hour12:false,hour:'2-digit',minute:'2-digit'}),d.toLocaleTimeString('en-US',{timeZone:'UTC',hour12:false})];}",
    json!([
        "1/1/1970, 12:00:00 AM",
        "1/1/1970",
        "12:00:00 AM",
        "1970/1/1 00:00:00",
        "00:00",
        "00:00:00"
    ])
);
js_test!(
    vm_stage8_locale_errors,
    "function main(){const d=new Date(0);let n=0;for(const args of [['fr'],['en',{timeZone:'Mars'}],['en',{month:'fake'}],['en',{calendar:'gregory'}]]){try{d.toLocaleString(...args);}catch(e){if(e instanceof RangeError)n++;}}return n;}",
    json!(4)
);
js_test!(
    vm_stage8_locale_24_hour_padding,
    "function main(){return [0,1,9,10,12,23].map(h=>{const d=new Date(h*3600000+184000);return [d.toLocaleTimeString('en-US',{timeZone:'UTC',hour12:false}),d.toLocaleTimeString('en-US',{timeZone:'UTC',hour12:false,hour:'numeric',minute:'numeric',second:'numeric'}),d.toLocaleTimeString('en-US',{timeZone:'UTC',hour12:false,hour:'numeric'}),d.toLocaleString('en-US',{timeZone:'UTC',hour12:false}),d.toLocaleTimeString('en-US',{timeZone:'UTC',hour12:true})];});}",
    json!([
        ["00:03:04", "00:03:04", "00", "1/1/1970, 00:03:04", "12:03:04 AM"],
        ["01:03:04", "01:03:04", "01", "1/1/1970, 01:03:04", "1:03:04 AM"],
        ["09:03:04", "09:03:04", "09", "1/1/1970, 09:03:04", "9:03:04 AM"],
        ["10:03:04", "10:03:04", "10", "1/1/1970, 10:03:04", "10:03:04 AM"],
        ["12:03:04", "12:03:04", "12", "1/1/1970, 12:03:04", "12:03:04 PM"],
        ["23:03:04", "23:03:04", "23", "1/1/1970, 23:03:04", "11:03:04 PM"]
    ])
);

fn saved(src: &str, replies: &[Value]) -> Value {
    let mut run = runtime(src);
    for reply in replies {
        assert!(matches!(run.next(), State::Action(_)));
        run = serde_json::from_value(serde_json::to_value(run).unwrap()).unwrap();
        run.resume(reply.clone(), false).unwrap();
    }
    returned(run)
}
#[test]
fn vm_stage8_snapshot_private_gc() {
    assert_eq!(
        saved(
            "function main(){function make(){return class{#x=7;static read(o){return o.#x;}m(){this.#x+=io.read({}).n;return this.#x;}};}const A=make(),a=new A();for(let n=0;n<800;n++){const trash={n};}const v=a.m();const B=make();let bad=false;try{B.read(a);}catch(e){bad=e instanceof TypeError;}return [v,A.read(a),bad,JSON.stringify(a)];}",
            &[json!({"n":2})]
        ),
        json!([9, 9, true, "{}"])
    );
}
#[test]
fn vm_stage8_snapshot_initializers() {
    assert_eq!(
        saved(
            "function main(){class A{x=io.read({}).n;constructor(p=io.read({}).n){this.p=p;}}class B extends A{#y=io.read({}).n;read(){return this.#y;}}const b=new B();return [b.x,b.p,b.read(),Object.keys(b)];}",
            &[json!({"n":1}), json!({"n":2}), json!({"n":3})]
        ),
        json!([1, 2, 3, ["x", "p"]])
    );
}
#[test]
fn vm_stage8_snapshot_static_and_accessor() {
    assert_eq!(
        saved(
            "function main(){class A{static #x=io.read({}).n;static{this.y=io.read({}).n;}get #v(){return io.read({}).n+A.#x;}read(){return this.#v;}}return [A.y,new A().read()];}",
            &[json!({"n":1}), json!({"n":2}), json!({"n":3})]
        ),
        json!([2, 4])
    );
}
#[test]
fn vm_stage8_snapshot_destructure() {
    assert_eq!(
        saved(
            "function main(){const o={};function key(){return io.read({}).key;}[o[key()]=io.read({}).n]=[];return o;}",
            &[json!({"key":"x"}), json!({"n":7})]
        ),
        json!({"x":7})
    );
}

js_test!(
    vm_stage8_regex_matchall_lastindex,
    "function main(){const r=/a(?=b)/g;r.lastIndex=2;return [Array.from('abab'.matchAll(r),m=>m.index),r.lastIndex];}",
    json!([[2], 2])
);
js_test!(
    vm_stage8_regex_sticky_consumers,
    "function main(){const r=/a(?=b)/y;r.lastIndex=2;const x='abab'.replace(r,'x'),n=r.lastIndex;r.lastIndex=2;const m='abab'.match(r);const g=/a(?=b)/gy;g.lastIndex=2;return [x,n,m.index,r.lastIndex,'abab'.match(g),g.lastIndex,Array.from('abab'.matchAll(g),m=>m.index),g.lastIndex];}",
    json!(["abxb", 3, 2, 3, ["a"], 0, [0], 0])
);
js_test!(
    vm_stage8_regex_replace_callback_sticky,
    "function main(){const r=/a(?=b)/y;r.lastIndex=2;const s='abab'.replace(r,()=>String(r.lastIndex));return [s,r.lastIndex];}",
    json!(["ab3b", 3])
);
js_test!(
    vm_stage8_regex_search_sticky,
    "function main(){const r=/(?<=a)b/y;r.lastIndex=1;return ['ab'.search(r),r.lastIndex];}",
    json!([-1, 1])
);
js_test!(
    vm_stage8_regex_backtrack_consumers,
    r#"function main(){const input='a'.repeat(64)+'!';let n=0;for(const method of ['test','exec','match','matchAll','replace','split','search']){const r=/^(a|aa)+\1$/g;try{if(method==='test'||method==='exec')r[method](input);else if(method==='replace')input.replace(r,'x');else input[method](r);}catch(e){if(e instanceof RangeError&&e.message.includes('backtrack'))n++;}}return n;}"#,
    json!(7)
);
js_test!(
    vm_stage8_private_nested_scope,
    "function main(){class A{#x=7;m(){class B{#x=8;m(){return this.#x;}}return [new B().m(),this.#x];}}return new A().m();}",
    json!([8, 7])
);
js_test!(
    vm_stage8_private_method_before_fields,
    "function main(){class A{x=this.#m();#m(){return 7;}static x=A.#s();static #s(){return 8;}}return [new A().x,A.x];}",
    json!([7, 8])
);
js_test!(
    vm_stage8_super_initializer_throw,
    "function main(){const log=[];class A{}class B extends A{x=(()=>{log.push('field');throw 7;})();constructor(){try{super();}catch(e){log.push(e,this instanceof B);}return this;}}new B();return log;}",
    json!(["field", 7, true])
);
#[test]
fn vm_stage8_async_callback_rejection() {
    let mut run = runtime(
        "async function main(){[1].forEach(async()=>{throw new Error('callback failed');});await 0;return 9;}",
    );
    let State::Failed(error) = run.next() else {
        panic!("unhandled async callback rejection must fail")
    };
    assert!(error.to_string().contains("callback failed"), "{error}");
}
#[test]
fn vm_stage8_snapshot_locale_getters() {
    assert_eq!(
        saved(
            "function main(){const options={get numeric(){return io.read({}).value;}};const result='2'.localeCompare('10','en',options);const date=new Date(0).toLocaleString('zh',{get timeZone(){return io.read({}).zone;}});return [result,date];}",
            &[json!({"value":true}), json!({"zone":"UTC"})]
        ),
        json!([-1, "1970/1/1 00:00:00"])
    );
}

#[test]
fn vm_stage8_private_duplicate_declarations() {
    for body in [
        "#x;#x;",
        "#x;static #x;",
        "get #x(){}get #x(){}",
        "get #x(){}static set #x(v){}",
    ] {
        assert!(
            compile(&format!("function main(){{class A{{{body}}}}}")).is_err(),
            "{body}"
        );
    }
}
#[test]
fn vm_stage8_snapshot_uri_coercion() {
    assert_eq!(
        saved(
            "function main(){const o={toString(){return io.read({}).s;}};return [encodeURI(o),decodeURI(o)];}",
            &[json!({"s":"a b"}), json!({"s":"a%20b"})]
        ),
        json!(["a%20b", "a b"])
    );
}
#[test]
fn vm_stage8_snapshot_raw_template() {
    assert_eq!(
        saved(
            "function main(){const t={get raw(){return io.read({}).raw;}};return String.raw(t,'x');}",
            &[json!({"raw":["a","b"]})]
        ),
        json!("axb")
    );
}
