use super::*;

fn with_host_replies(source: &str, replies: &[(&str, Value)]) -> Value {
    let mut vm = runtime(source);
    for (path, reply) in replies {
        let State::Action(action) = vm.next() else { panic!("expected io.read({path})") };
        assert!(matches!(action.kind, ActionKind::Tool { ref name, ref args }
            if name == "read_file" && args["path"] == *path), "{action:?}");
        vm = serde_json::from_value(serde_json::to_value(vm).unwrap()).unwrap();
        vm.resume(reply.clone(), false).unwrap();
    }
    returned(vm)
}

#[test]
fn group_by_keys_and_iteration() {
    let result = returned(runtime(r#"
function main() {
  const sym = Symbol('group'), convert = {[Symbol.toPrimitive](hint) { assert(hint === 'string'); return sym; }};
  const group = Object.groupBy([10, , 30, 40], function (x, i) {
    assert(this === undefined); return i % 2 === 0 ? convert : '__proto__';
  });
  const key = {}, source = { *[Symbol.iterator]() { yield 1; yield 2; yield 3; yield 4; } };
  const mapGroupBy = Map.groupBy;
  const map = mapGroupBy(source, (x, i) => [-0, +0, NaN, key][i]);
  return [Object.getPrototypeOf(group) === null, group[sym], group.__proto__,
    map instanceof Map, map.get(0), map.get(NaN), map.get(key), map.size,
    Object.is(map.keys().next().value, 0), Object.groupBy([], () => 'x'), Map.groupBy([], () => 0).size];
}
"#));
    assert_eq!(result, json!([true, [10, 30], [null, 40], true, [1, 2], [3], [4], 3, true, {}, 0]));
}

#[test]
fn group_by_closes_iterator_on_callback_or_key_error() {
    let result = returned(runtime(r#"
function main() {
  const log = [], failure = {}, source = {[Symbol.iterator]() {
    log.push('open'); return {next() { log.push('next'); return {value: 1}; },
      return() { log.push('close'); throw 'ignored'; }};
  }};
  try { Object.groupBy(source, () => ({[Symbol.toPrimitive]() { throw failure; }})); } catch (e) { log.push(e === failure); }
  try { Map.groupBy(source, () => { throw failure; }); } catch (e) { log.push(e === failure); }
  try { Object.groupBy(source, null); } catch (e) { log.push(e instanceof TypeError); }
  try { Map.groupBy({}, () => 0); } catch (e) { log.push(e instanceof TypeError); }
  try { Map.groupBy(); } catch (e) { log.push(e instanceof TypeError); }
  return log;
}
"#));
    assert_eq!(result, json!(["open", "next", "close", true, "open", "next", "close", true, true, true, true]));
}

#[test]
fn group_by_snapshots_inside_both_callbacks() {
    let result = with_host_replies(r#"
function main() {
  const source = {*[Symbol.iterator]() { yield 'a'; yield 'b'; }};
  const byObject = Object.groupBy(source, (x, i) => io.read({path: 'o' + x}).key + i);
  const byMap = Map.groupBy(source, (x, i) => io.read({path: 'm' + x}).key - i);
  return [byObject, [...byMap]];
}
"#, &[("oa", json!({"key":"group"})), ("ob", json!({"key":"group"})),
      ("ma", json!({"key":5})), ("mb", json!({"key":6}))]);
    assert_eq!(result, json!([{"group0":["a"], "group1":["b"]}, [[5, ["a", "b"]]]]));
}

#[test]
fn immutable_array_methods_densify_and_respect_bounds() {
    let result = returned(runtime(r#"
function main() {
  const a = [, 'b', , 'd'];
  const clean = a.toSpliced(), edit = a.toSpliced(-2, 1, 'C', '!'), updated = a.with(-2, 'C');
  const errors = [];
  for (const i of [4, -5, Infinity, -Infinity]) {
    try { a.with(i, 0); } catch (e) { errors.push(e instanceof RangeError); }
  }
  try { [].with(NaN, 0); } catch (e) { errors.push(e instanceof RangeError); }
  try { a.toSpliced(Symbol()); } catch (e) { errors.push(e instanceof TypeError); }
  const borrowed = Array.prototype.with.call({length: 2, 1: 'b', get 0() { throw 'do not read replaced index'; }}, NaN, 'A');
  return [Object.keys(a), Object.keys(clean), Object.keys(edit), Object.keys(updated),
    clean, edit, updated, a.toSpliced(-2), a.toSpliced(1, undefined, 'I'), a.with(NaN, 'A'),
    borrowed, a.with.length, a.toSpliced.length, errors];
}
"#));
    assert_eq!(result, json!([
        ["1", "3"], ["0", "1", "2", "3"], ["0", "1", "2", "3", "4"], ["0", "1", "2", "3"],
        [null,"b",null,"d"], [null,"b","C","!","d"], [null,"b","C","d"], [null,"b"],
        [null,"I","b",null,"d"], ["A","b",null,"d"], ["A","b"], 2, 2, [true,true,true,true,true,true]
    ]));
}

#[test]
fn promise_with_resolvers_round_trips_pending_capabilities() {
    let result = with_host_replies(r#"
async function main() {
  const a = Promise.withResolvers(), {promise, resolve, reject} = Promise.withResolvers();
  const caught = promise.catch(e => e);
  const receipt = io.read({path: 'before-settle'});
  a.resolve({then(r) { r(receipt.value); }});
  a.reject('ignored');
  reject('rejected'); resolve('ignored');
  class P extends Promise {}
  const b = P.withResolvers(); b.resolve('subclass');
  let typeError = false; try { (0, Promise.withResolvers)(); } catch (e) { typeError = e instanceof TypeError; }
  return [Object.keys(a), a.promise instanceof Promise, await a.promise, await caught,
    typeError, b.promise instanceof P, await b.promise, Promise.withResolvers.length];
}
"#, &[("before-settle", json!({"value":42}))]);
    assert_eq!(result, json!([["promise", "resolve", "reject"], true, 42, "rejected", true, true, "subclass", 0]));
}

#[test]
fn set_operations_preserve_order_and_accept_set_like_objects() {
    let result = returned(runtime(r#"
function main() {
  const a = new Set([3, 2, 1]), b = new Set([1, 2, 4, 5]);
  const key = {}, map = new Map([[1, 'value'], [key, 'other']]);
  const like = {size: '2.9', has(x) { assert(this === like); return x === 1 || x === 2; },
    keys() { assert(this === like); let i = 0; const a = [1, 1, 2]; return {next() { return {value:a[i],done:i++ >= a.length}; }}; }};
  return [[...a.union(b)], [...a.intersection(b)], [...a.intersection(like)],
    [...a.difference(like)], [...a.difference(b)], [...a.symmetricDifference(like)], [...a.symmetricDifference(map)].map(x=>x===key?'key':x),
    new Set([2,1]).isSubsetOf(like), a.isSubsetOf(b), a.isSupersetOf(like), like.has(3),
    a.isDisjointFrom(like), a.isDisjointFrom(new Set([9])), [...a], [...b],
    Set.prototype.union.length, Object.getOwnPropertyDescriptor(Set.prototype, 'union').enumerable];
}
"#));
    assert_eq!(result, json!([[3,2,1,4,5],[2,1],[1,2],[3],[3],[3],[3,2,"key"],true,false,true,false,false,true,[3,2,1],[1,2,4,5],1,false]));
}

#[test]
fn set_operations_validate_set_like_and_close_early() {
    let result = returned(runtime(r#"
function main() {
  const set = new Set([1,2]), log = [];
  const like = {get size() { log.push('size'); return 1; }, get has() { log.push('has'); return () => false; },
    get keys() { log.push('keys'); return function () { log.push('open');
      return {next() { log.push('step'); return {value: this.value || 9}; }, value: this.value,
        return() { log.push('close'); return {}; }};
    }; }, value: 9};
  log.push(set.isSupersetOf(like)); like.value = 1;
  log.push(set.isDisjointFrom(like));
  try { Set.prototype.union.call({}, like); } catch (e) { log.push(e instanceof TypeError); }
  try { set.union({size:-1}); } catch (e) { log.push(e instanceof RangeError); }
  try { set.isSubsetOf({size:'NaN'}); } catch (e) { log.push(e instanceof TypeError); }
  try { set.isSubsetOf({size:0, has(){}}); } catch (e) { log.push(e instanceof TypeError); }
  try { set.union({size:0, has:0, keys(){}}); } catch (e) { log.push(e instanceof TypeError); }
  try { set.union({size:0, has(){}, keys(){return 0;}}); } catch (e) { log.push(e instanceof TypeError); }
  return log;
}
"#));
    assert_eq!(result, json!(["size","has","keys","open","step","close",false,
        "size","has","keys","open","step","close",false,true,true,true,true,true,true]));
}

#[test]
fn set_like_callbacks_can_suspend() {
    let result = with_host_replies(r#"
function main() {
  const other = {size: Infinity, keys(){throw 'unexpected keys';}, has(x){return io.read({path:'key-' + x}).present;}};
  return [...new Set([1,2]).intersection(other)];
}
"#, &[("key-1", json!({"present": false})), ("key-2", json!({"present": true}))]);
    assert_eq!(result, json!([2]));
}

#[test]
fn regexp_escape_literals_and_errors() {
    let result = returned(runtime(r#"
function main() {
  const escape = RegExp.escape;
  const source = 'C:\\tmp/my-file.[js]?#\n\r\t\f\v\u2028\u00A0💡';
  const re = new RegExp('^' + escape(source) + '$', 'u');
  const errors = [];
  for (const value of [null, 1, {}, String, undefined]) {
    try { escape(value); } catch (e) { errors.push(e instanceof TypeError); }
  }
  return [re.test(source), re.test(source.replace('tmp', 'x')), escape('0x +-*'), escape('\uD800'),
    escape(''), escape.length, errors];
}
"#));
    assert_eq!(result, json!([true, false, r"\x30x\x20\+\x2d\*", r"\ud800", "", 1, [true,true,true,true,true]]));
}

#[test]
fn unsupported_builtin_diagnostics_are_scoped() {
    for (source, expected, forbidden) in [
        ("Object.nonExistent({});", "unsupported built-in Object.nonExistent in the flow runtime.", &["Buffer", "URL", "path", "require", "read_program_guide"][..]),
        ("url.resolve('a', 'b');", "Use new URL(input, base)", &["Buffer", "isAbsolute", "require", "read_program_guide"]),
        ("path.matchesGlob('/tmp', '*');", "Supported path operations:", &["Buffer", "new URL", "require", "read_program_guide"]),
        ("require.resolve('io');", "Use require(name)", &["Buffer", "new URL", "isAbsolute", "read_program_guide"]),
    ] {
        let error = compile(source).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
        for text in forbidden { assert!(!error.contains(text), "{error}"); }
    }
}
