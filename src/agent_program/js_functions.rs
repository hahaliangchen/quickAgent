//! Function intrinsics and receiver-aware method values. Prototype/descriptors
//! are a later stage; these are the methods already implemented by the VM.
use super::heap::{Heap, HeapNode, VmValue};
use anyhow::Result;

pub(super) const ARRAY: &str = "at includes indexOf lastIndexOf slice concat push pop shift unshift splice reverse join flat fill sort toSorted toReversed keys values entries map filter forEach some every find findIndex findLast findLastIndex flatMap reduce reduceRight";
pub(super) const STRING: &str = "at includes indexOf lastIndexOf slice concat startsWith endsWith trim trimStart trimEnd toLowerCase toUpperCase toLocaleLowerCase toLocaleUpperCase split replace replaceAll match matchAll search charAt charCodeAt codePointAt substring substr repeat padStart padEnd";
pub(super) const DATE: &str = "getTime toISOString toJSON getFullYear getMonth getDate getDay getHours getMinutes getSeconds getMilliseconds getUTCFullYear getUTCMonth getUTCDate getUTCDay getUTCHours getUTCMinutes getUTCSeconds getUTCMilliseconds";
fn contains(list: &str, name: &str) -> bool {
    list.split_whitespace().any(|item| item == name)
}

pub(super) fn install(heap: &mut Heap) -> Result<()> {
    for name in ["call", "apply", "bind"] {
        heap.native(&format!("Function.{name}"))?;
    }
    for (kind, methods) in [
        ("Array", ARRAY),
        ("String", STRING),
        ("Date", DATE),
        ("RegExp", "test exec"),
        ("Number", "toFixed toExponential"),
        ("Promise", "then catch finally"),
        ("Object", "toString valueOf hasOwnProperty"),
    ] {
        for name in methods.split_whitespace() {
            heap.native(&format!("method:{kind}.{name}"))?;
        }
    }
    Ok(())
}

pub(super) fn method(heap: &Heap, receiver: &VmValue, name: &str) -> Option<VmValue> {
    let kind = match receiver {
        VmValue::Reference(id) => match heap.get(*id).ok()? {
            HeapNode::Closure { .. }
            | HeapNode::BoundFunction { .. }
            | HeapNode::Native(_)
            | HeapNode::PromiseResolver { .. }
                if matches!(name, "call" | "apply" | "bind") =>
            {
                return heap.native_reference(&format!("Function.{name}"));
            }
            HeapNode::Array(_) if contains(ARRAY, name) => "Array",
            HeapNode::Promise { .. } if matches!(name, "then" | "catch" | "finally") => "Promise",
            HeapNode::Object(_) if heap.date_value(*id).is_some() && contains(DATE, name) => "Date",
            HeapNode::Object(_)
                if heap.regex_parts(*id).is_some() && matches!(name, "test" | "exec") =>
            {
                "RegExp"
            }
            _ if matches!(name, "toString" | "valueOf" | "hasOwnProperty") => "Object",
            _ => return None,
        },
        VmValue::String(_) | VmValue::Utf16(_) if contains(STRING, name) => "String",
        VmValue::Number(_) if matches!(name,"toFixed"|"toExponential") => "Number",
        VmValue::Undefined | VmValue::Null => return None,
        _ if matches!(name, "toString" | "valueOf" | "hasOwnProperty") => "Object",
        _ => return None,
    };
    heap.native_reference(&format!("method:{kind}.{name}"))
}

pub(super) fn native_length(id: &str) -> usize {
    match id {
        "Symbol"=>return 0,"Symbol.for"|"Symbol.keyFor"|"Symbol.toPrimitive"|"Function.hasInstance"=>return 1,
        "Object.defineProperty"|"Reflect.defineProperty"|"Reflect.apply"|"Reflect.set"=>return 3,
        "Object.defineProperties"|"Object.getOwnPropertyDescriptor"|"Reflect.getOwnPropertyDescriptor"|"Reflect.get"|"Reflect.construct"|"Reflect.has"|"Reflect.deleteProperty"|"Reflect.setPrototypeOf"=>return 2,
        "Object.getOwnPropertyDescriptors"|"Object.getOwnPropertyNames"|"Object.getOwnPropertySymbols"|"Object.preventExtensions"|"Object.isExtensible"|"Object.seal"|"Object.isSealed"|"Object.isFrozen"|"Reflect.ownKeys"|"Reflect.getPrototypeOf"|"Reflect.isExtensible"|"Reflect.preventExtensions"=>return 1,
        _=>{},
    }
    let method = id.strip_prefix("method:").unwrap_or(id);
    if id.starts_with("tool:") {
        return 1;
    }
    if let Some((_, name)) = id.split_once(".prototype.") {
        return match name {
            "set" => 2,
            "get" | "has" | "delete" | "add" | "forEach" => 1,
            _ => 0,
        };
    }
    if method.starts_with("Math.") {
        return match method {
            "Math.random" => 0,
            "Math.max" | "Math.min" | "Math.pow" | "Math.atan2" | "Math.imul" | "Math.hypot" => 2,
            _ => 1,
        };
    }
    match method {
        "Object" | "Function" | "Promise" => 1,
        "RegExp" => 2,
        "Error" | "TypeError" | "RangeError" | "ReferenceError" | "SyntaxError" | "EvalError"
        | "URIError" => 1,
        "AggregateError" => 2,
        "Object.create" | "Object.setPrototypeOf" => 2,
        "Object.getPrototypeOf" | "Object.isPrototypeOf" => 1,
        "Promise.resolve" | "Promise.reject" | "Promise.all" | "Promise.allSettled"
        | "Promise.race" | "Promise.any" => 1,
        "Function.call" | "Function.bind" => 1,
        "Function.apply" => 2,
        "String" | "Number" | "Boolean" | "parseFloat" | "isNaN" | "isFinite"
        | "encodeURIComponent" | "decodeURIComponent" | "structuredClone" | "queueMicrotask"
        | "clearTimeout" | "clearInterval" => 1,
        "parseInt" | "setTimeout" | "setInterval" | "JSON.parse" | "Object.is"
        | "Object.hasOwn" | "Object.assign" => 2,
        "JSON.stringify" => 3,
        "Array.of" | "Date.now" => 0,
        "Date.UTC" => 7,
        "Date.parse" | "Object.keys" | "Object.values" | "Object.entries"
        | "Object.fromEntries" | "Object.freeze" | "Array.isArray" => 1,
        "String.fromCharCode" | "String.fromCodePoint" | "String.raw" => 1,
        "Number.isNaN"
        | "Number.isFinite"
        | "Number.isInteger"
        | "Number.isSafeInteger"
        | "Number.parseFloat" => 1,
        "Number.parseInt" => 2,
        "Array" => 1,
        "Date" => 7,
        "Array.from" => 1,
        "Array.slice" | "String.slice" | "String.substring" | "String.substr" | "Array.splice"
        | "String.replace" | "String.replaceAll" | "String.split" => 2,
        "Array.concat" | "String.concat" | "Array.push" | "Array.unshift" => 1,
        "Array.map"
        | "Array.filter"
        | "Array.forEach"
        | "Array.some"
        | "Array.every"
        | "Array.find"
        | "Array.findIndex"
        | "Array.findLast"
        | "Array.findLastIndex"
        | "Array.flatMap"
        | "Array.reduce"
        | "Array.reduceRight"
        | "Array.sort"
        | "Array.toSorted"
        | "Array.join"
        | "String.charAt"
        | "String.charCodeAt"
        | "String.codePointAt"
        | "String.padStart"
        | "String.padEnd"
        | "Number.toFixed"
        | "Number.toExponential"
        | "Object.hasOwnProperty"
        | "Date.toJSON"
        | "Promise.catch"
        | "Promise.finally" => 1,
        "Promise.then" => 2,
        _ => super::js_methods::builtin_arity(id)
            .map(|(min, _)| min)
            .or_else(|| {
                super::js_methods::method_arity(method.rsplit('.').next().unwrap_or(method))
                    .map(|(min, _)| min)
            })
            .unwrap_or(0),
    }
}

pub(super) fn apply_args(heap: &Heap, value: VmValue) -> Result<Vec<VmValue>> {
    use anyhow::{bail, ensure};
    if matches!(value, VmValue::Undefined | VmValue::Null) {
        return Ok(vec![]);
    }
    ensure!(
        matches!(value, VmValue::Reference(_)),
        "TypeError: apply arguments must be an object, null or undefined"
    );
    let length = super::vm_eval::member(heap, &value, &VmValue::String("length".into()))?;
    let length = super::js_value::to_number(heap, &length)?;
    let length = if length.is_nan() || length < 0.0 {
        0.0
    } else {
        length.floor()
    };
    if length > 1_000_000.0 {
        bail!("RangeError: apply arguments exceed the runtime array limit");
    }
    (0..length as usize)
        .map(|index| super::vm_eval::member(heap, &value, &VmValue::String(index.to_string())))
        .collect()
}

/// Builtins which may execute user accessors use ordinary durable VM frames.
pub(super) fn resumable_builtin(name:&str)->Option<&'static str>{Some(match name {
    "Number"=>"$$numberBuiltin","new Date"=>"$$constructDate",
    "String"=>"$$stringBuiltin","Symbol"=>"$$symbolConvert","Symbol.for"=>"$$symbolForConvert",
    "Object.getOwnPropertyDescriptor"=>"$$objectDescriptor",
    "Reflect.has"=>"$$reflectHas",
    "Reflect.deleteProperty"=>"$$reflectDelete",
    "Reflect.getOwnPropertyDescriptor"=>"$$reflectDescriptor",
    "new Map"=>"$$constructMap","new Set"=>"$$constructSet","new WeakMap"=>"$$constructWeakMap","new WeakSet"=>"$$constructWeakSet",
    "Object.fromEntries"=>"$$fromEntries",
    "$$getIterator"=>"$$getIterator","$$iteratorStep"=>"$$iteratorStep","$$iteratorCloseNormal"=>"$$iteratorCloseNormal","$$iteratorCloseThrow"=>"$$iteratorCloseThrow",
    "$$iterator"=>"$$getIterator","$$iterableArray"=>"$$iterableToArray",
    "Array.from"=>"$$arrayFrom",
    "$$iteratorValue"=>"$$iteratorRead",
    "Object.defineProperty"=>"$$defineProperty", "Reflect.defineProperty"=>"$$reflectDefineProperty",
    "Object.defineProperties"=>"$$defineProperties", "Object.create"=>"$$objectCreate",
    "Object.values"=>"$$objectValues", "Object.entries"=>"$$objectEntries", "Object.assign"=>"$$objectAssign",
    "Object.getOwnPropertyDescriptors"=>"$$objectDescriptors", "JSON.stringify"=>"$$jsonStringify",
    "Reflect.get"=>"$$reflectGet", "Reflect.set"=>"$$reflectSet", "Reflect.apply"=>"$$reflectApply", "Reflect.construct"=>"$$reflectConstruct",
    "$$objectRest"=>"$$objectRest",
    "$$propertyKey"=>"$$toPropertyKey",
    "$$sortWriteBack"=>"$$sortApply",
    "$$setProperty"=>"$$strictSet", "$$spread"=>"$$spreadObject",
    _=>return None,
})}
