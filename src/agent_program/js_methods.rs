//! Built-in methods and global functions over VM values, following the
//! ECMAScript algorithms. Strings are indexed by Unicode scalar values (a
//! documented difference from UTF-16 code units); everything else matches the
//! specification for the value kinds this runtime has.
use super::heap::{Heap, HeapId, HeapNode, JsNumber, VmValue};
use super::js_value::{
    integer_or_infinity, is_js_whitespace, number_to_string, relative_position, strict_equal,
    string_to_number, to_number, to_string,
};
use anyhow::{Result, bail, ensure};
use std::collections::BTreeSet;

const TEXT_RESULT_LIMIT: usize = 4 * 1024 * 1024;
const ARRAY_LIMIT: usize = 1_000_000;
const REGEX_RESULT_LIMIT: usize = 10_000;

fn arg(args: &[VmValue], index: usize) -> VmValue {
    args.get(index).cloned().unwrap_or(VmValue::Undefined)
}
fn number(value: f64) -> VmValue {
    VmValue::Number(JsNumber::new(value))
}
fn text_result(value: String) -> Result<VmValue> {
    ensure!(value.len() <= TEXT_RESULT_LIMIT, "RangeError: string result exceeds 4 MiB");
    Ok(VmValue::String(value))
}
fn new_array(heap: &mut Heap, items: Vec<VmValue>) -> Result<VmValue> {
    ensure!(items.len() <= ARRAY_LIMIT, "RangeError: array exceeds the runtime limit");
    Ok(VmValue::Reference(heap.allocate(HeapNode::Array(items))?))
}
fn array_items(heap: &Heap, id: HeapId) -> Result<Vec<VmValue>> {
    match heap.get(id)? {
        HeapNode::Array(items) => {
            for i in 0..items.len(){ensure!(heap.descriptor_owner(id,&i.to_string())?.is_none_or(|(_,d)|d.accessor.is_none()),"TypeError: accessor needs resumable evaluation in this array builtin; materialize values with Array.from first");}
            Ok(items.clone())
        },
        _ => bail!("TypeError: receiver is not an array"),
    }
}
/// SameValueZero, used by includes.
fn same_value_zero(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::Number(a), VmValue::Number(b)) if a.get().is_nan() && b.get().is_nan() => true,
        _ => strict_equal(a, b),
    }
}

/// Method names a compiled program may call, with their argument counts.
/// The receiver kind is only known at run time, so this is the union.
pub(super) fn method_arity(name: &str) -> Option<(usize, usize)> {
    Some(match name {
        "localeCompare" => (1,3),
        "toLocaleString" | "toLocaleDateString" | "toLocaleTimeString" => (0,2),
        "at" | "repeat" | "search" | "match" | "matchAll" | "test" | "exec" => (1, 1),
        "charAt" | "charCodeAt" | "codePointAt" | "toFixed" | "toExponential" | "join" | "flat" | "toString"
        | "toLowerCase" | "toUpperCase" | "toLocaleLowerCase" | "toLocaleUpperCase"
        => (0, 1),
        "includes" | "indexOf" | "lastIndexOf" | "startsWith" | "endsWith" | "padStart"
        | "padEnd" => (1, 2),
        "slice" | "split" | "substring" | "substr" => (0, 2),
        "replace" | "replaceAll" => (2, 2),
        "trim" | "trimStart" | "trimEnd" | "valueOf" | "pop" | "shift" | "reverse" | "keys"
        | "values" | "entries" | "toReversed" => (0, 0),
        "concat" | "push" | "unshift" | "splice" => (0, 64),
        "fill" => (1, 3),
        "sort" | "toSorted" => (0, 1),
        "getTime" | "toISOString" | "toJSON" | "getFullYear" | "getMonth" | "getDate"
        | "getDay" | "getHours" | "getMinutes" | "getSeconds" | "getMilliseconds"
        | "getUTCFullYear" | "getUTCMonth" | "getUTCDate" | "getUTCDay" | "getUTCHours"
        | "getUTCMinutes" | "getUTCSeconds" | "getUTCMilliseconds" => (0, 0),
        _ => return None,
    })
}

/// Explain unavailable methods that a model is likely to reach for.
pub(super) fn unavailable_method(name: &str) -> Option<&'static str> {
    Some(match name {
        "normalize" => "String.prototype.normalize is unavailable in the flow runtime",
        _ => return None,
    })
}

fn regex_parts(heap: &Heap, value: &VmValue) -> Option<(String, String)> {
    match value {
        VmValue::Reference(id) => heap.regex_parts(*id),
        _ => None,
    }
}

/// Byte offsets from the regex engine become Unicode scalar positions.
fn char_index(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

/// GetSubstitution: $$, $&, $`, $', $n, $nn and $<name> in a replacement string.
fn substitution(
    replacement: &str,
    text: &str,
    start: usize,
    end: usize,
    captures: &[Option<String>],
    names: &[(String, Option<String>)],
) -> String {
    let matched = &text[start..end];
    let chars: Vec<char> = replacement.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' || i + 1 == chars.len() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let next = chars[i + 1];
        match next {
            '$' => {
                out.push('$');
                i += 2;
            }
            '&' => {
                out.push_str(matched);
                i += 2;
            }
            '`' => {
                out.push_str(&text[..start]);
                i += 2;
            }
            '\'' => {
                out.push_str(&text[end..]);
                i += 2;
            }
            '0'..='9' => {
                let first = next.to_digit(10).unwrap() as usize;
                let second = chars.get(i + 2).and_then(|c| c.to_digit(10)).map(|d| d as usize);
                let count = captures.len();
                if let Some(second) = second
                    && (1..=count).contains(&(first * 10 + second))
                {
                    out.push_str(captures[first * 10 + second - 1].as_deref().unwrap_or(""));
                    i += 3;
                } else if (1..=count).contains(&first) {
                    out.push_str(captures[first - 1].as_deref().unwrap_or(""));
                    i += 2;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            '<' if !names.is_empty() => {
                let rest: String = chars[i + 2..].iter().collect();
                if let Some(close) = rest.find('>') {
                    let name = &rest[..close];
                    let value = names
                        .iter()
                        .find(|(group, _)| group == name)
                        .and_then(|(_, value)| value.clone())
                        .unwrap_or_default();
                    out.push_str(&value);
                    i += 2 + name.chars().count() + 1;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

fn captures_of(captures: &super::values::JsCaptures<'_>) -> Vec<Option<String>> {
    captures
        .iter()
        .skip(1)
        .map(|m| m.map(|m| m.as_str().to_owned()))
        .collect()
}
fn names_of(re: &super::values::JsRegex, captures: &super::values::JsCaptures<'_>) -> Vec<(String, Option<String>)> {
    re.capture_names()
        .flatten()
        .map(|name| (name.to_owned(), captures.name(name).map(|m| m.as_str().to_owned())))
        .collect()
}

/// The match array of RegExpBuiltinExec, with undefined for unmatched groups.
fn match_array(heap: &mut Heap, re: &super::values::JsRegex, captures: &super::values::JsCaptures<'_>, input: &str) -> Result<VmValue> {
    ensure!(
        captures.len() <= REGEX_RESULT_LIMIT,
        "RangeError: regex capture result exceeds 10000 items"
    );
    let items = captures
        .iter()
        .map(|m| match m {
            Some(m) => VmValue::String(m.as_str().to_owned()),
            None => VmValue::Undefined,
        })
        .collect();
    let value = new_array(heap, items)?;
    let VmValue::Reference(id) = value else { unreachable!() };
    let names = names_of(re, captures);
    let groups = if names.is_empty() { VmValue::Undefined } else {
        let fields = names.into_iter().map(|(key, value)| (key, value.map(VmValue::String).unwrap_or(VmValue::Undefined))).collect();
        VmValue::Reference(heap.allocate(HeapNode::Object(fields))?)
    };
    heap.set_property(id, "groups".into(), groups)?;
    heap.set_property(id, "index".into(), number(char_index(input, captures.get(0).unwrap().start()) as f64))?;
    heap.set_property(id, "input".into(), VmValue::String(input.to_owned()))?;
    Ok(value)
}

/// A string pattern used where a RegExp is expected becomes `new RegExp(pattern)`.
fn regex_argument(heap: &Heap, value: &VmValue) -> Result<(super::values::JsRegex, String)> {
    let (pattern, flags) = match regex_parts(heap, value) {
        Some(parts) => parts,
        None => (to_string(heap, value)?, String::new()),
    };
    Ok((super::values::compile_regex(&pattern, &flags)?, flags))
}

/// BuiltinExec state is shared by test/exec and every string consumer.
fn regex_exec<'t>(heap: &mut Heap, id: HeapId, re: &super::values::JsRegex, flags: &str, input: &'t str) -> Result<Option<super::values::JsCaptures<'t>>> {
    let stateful=flags.contains('g')||flags.contains('y');
    let start=if stateful { let n=to_number(heap,&heap.regex_last_index(id))?; if n.is_nan()||n<=0.0 {0} else {n.floor().min(usize::MAX as f64) as usize} } else {0};
    let offset=if start==0 {Some(0)} else if start==input.chars().count() {Some(input.len())} else {input.char_indices().nth(start).map(|(i,_)|i)};
    let captures=if let Some(offset)=offset { re.captures_at(input,offset)?.filter(|c|!flags.contains('y')||c.get(0).is_some_and(|m|m.start()==offset)) } else {None};
    if stateful { let end=captures.as_ref().and_then(|c|c.get(0)).map(|m|char_index(input,m.end())).unwrap_or(0);heap.set_property(id,"lastIndex".into(),number(end as f64))?; }
    Ok(captures)
}
fn regex_id(heap: &mut Heap, value: &VmValue) -> Result<HeapId> {
    if let VmValue::Reference(id)=value && heap.regex_parts(*id).is_some() { return Ok(*id); }
    let VmValue::Reference(id)=construct_regexp(heap,&[value.clone()])? else {unreachable!()};Ok(id)
}
fn regex_collect<'t>(heap: &mut Heap, id: HeapId, re: &super::values::JsRegex, flags: &str, text: &'t str, global: bool, reset: bool) -> Result<Vec<super::values::JsCaptures<'t>>> {
    if reset {heap.set_property(id,"lastIndex".into(),number(0.0))?;}
    let mut out=Vec::new();let mut captured_bytes=0usize;
    while let Some(c)=regex_exec(heap,id,re,flags,text)? {
        ensure!(out.len()<REGEX_RESULT_LIMIT,"RangeError: regex match result exceeds 10000 items");
        captured_bytes+=c.iter().flatten().map(|m|m.as_str().len()).sum::<usize>();
        ensure!(captured_bytes<=TEXT_RESULT_LIMIT,"RangeError: regex capture result exceeds 4 MiB");
        let whole=c.get(0).unwrap();
        if global && whole.start()==whole.end() { heap.set_property(id,"lastIndex".into(),number((char_index(text,whole.end())+1) as f64))?; }
        out.push(c);
        if !global {break;}
    }
    Ok(out)
}

fn replace(heap: &mut Heap, text: &str, name: &str, args: &[VmValue]) -> Result<VmValue> {
    let pattern = arg(args, 0);
    let replacement = arg(args, 1);
    if matches!(replacement, VmValue::Reference(id) if matches!(heap.get(id), Ok(HeapNode::Closure { .. })))
    {
        bail!("TypeError: replacement functions are not available yet; compute the replacement with a loop over matches");
    }
    let replacement = to_string(heap, &replacement)?;
    let mut out = String::new();
    if let Some((source, flags)) = regex_parts(heap, &pattern) {
        let global = flags.contains('g');
        ensure!(
            name == "replace" || global,
            "TypeError: replaceAll must be called with a global RegExp"
        );
        let re = super::values::compile_regex(&source, &flags)?;
        let mut last = 0;
        let id=regex_id(heap,&pattern)?;
        for captures in regex_collect(heap,id,&re,&flags,text,global,global)? {
            let whole = captures.get(0).unwrap();
            out.push_str(&text[last..whole.start()]);
            out.push_str(&substitution(
                &replacement,
                text,
                whole.start(),
                whole.end(),
                &captures_of(&captures),
                &names_of(&re, &captures),
            ));
            last = whole.end();
            ensure!(out.len() <= TEXT_RESULT_LIMIT, "RangeError: string result exceeds 4 MiB");
            if !global {
                break;
            }
        }
        out.push_str(&text[last..]);
        return text_result(out);
    }
    let search = to_string(heap, &pattern)?;
    let mut positions = Vec::new();
    if name == "replace" {
        if let Some(position) = text.find(&search) {
            positions.push(position);
        }
    } else if search.is_empty() {
        positions.push(0);
        positions.extend(text.char_indices().map(|(i, c)| i + c.len_utf8()));
    } else {
        positions.extend(text.match_indices(&search).map(|(i, _)| i));
    }
    let mut last = 0;
    for position in positions {
        out.push_str(&text[last..position]);
        out.push_str(&substitution(&replacement, text, position, position + search.len(), &[], &[]));
        last = position + search.len();
        ensure!(out.len() <= TEXT_RESULT_LIMIT, "RangeError: string result exceeds 4 MiB");
    }
    out.push_str(&text[last..]);
    text_result(out)
}

fn split(heap: &mut Heap, text: &str, args: &[VmValue]) -> Result<VmValue> {
    let separator = arg(args, 0);
    let limit = match arg(args, 1) {
        VmValue::Undefined => u32::MAX as usize,
        value => {
            let n = to_number(heap, &value)?;
            if n.is_finite() { (n.trunc().rem_euclid(4294967296.0)) as usize } else { 0 }
        }
    };
    let mut parts: Vec<VmValue> = Vec::new();
    if limit == 0 {
        return new_array(heap, parts);
    }
    if matches!(separator, VmValue::Undefined) {
        return new_array(heap, vec![VmValue::String(text.to_owned())]);
    }
    if let Some((source, flags)) = regex_parts(heap, &separator) {
        let re = super::values::compile_regex(&source, &flags)?;
        if text.is_empty() {
            return new_array(
                heap,
                if re.is_match("")? { vec![] } else { vec![VmValue::String(String::new())] },
            );
        }
        let mut last = 0;
        for captures in re.captures_iter(text) {
            let captures = captures?;
            let whole = captures.get(0).unwrap();
            // An empty match at the previous split point or at the end does
            // not split, exactly as the specification's SplitMatcher loop.
            if whole.start() == whole.end() && (whole.start() == last || whole.start() == text.len()) {
                continue;
            }
            if whole.end() == whole.start() && whole.start() >= text.len() {
                continue;
            }
            parts.push(VmValue::String(text[last..whole.start()].to_owned()));
            if parts.len() == limit {
                return new_array(heap, parts);
            }
            for capture in captures.iter().skip(1) {
                parts.push(match capture {
                    Some(m) => VmValue::String(m.as_str().to_owned()),
                    None => VmValue::Undefined,
                });
                if parts.len() == limit {
                    return new_array(heap, parts);
                }
            }
            last = whole.end();
            ensure!(parts.len() <= ARRAY_LIMIT, "RangeError: split result exceeds the runtime limit");
        }
        parts.push(VmValue::String(text[last..].to_owned()));
        return new_array(heap, parts);
    }
    let separator = to_string(heap, &separator)?;
    let pieces: Vec<String> = if separator.is_empty() {
        text.chars().map(|c| c.to_string()).collect()
    } else {
        text.split(separator.as_str()).map(str::to_owned).collect()
    };
    parts.extend(pieces.into_iter().take(limit).map(VmValue::String));
    new_array(heap, parts)
}

/// String.prototype methods. Positions count Unicode scalar values.
fn string_method(heap: &mut Heap, text: &str, name: &str, args: &[VmValue]) -> Result<VmValue> {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let position = |heap: &Heap, value: &VmValue, default: usize| -> Result<usize> {
        Ok(match value {
            VmValue::Undefined => default,
            value => integer_or_infinity(to_number(heap, value)?).clamp(0.0, len as f64) as usize,
        })
    };
    let search_string = |heap: &Heap, value: &VmValue| -> Result<Vec<char>> {
        ensure!(
            regex_parts(heap, value).is_none(),
            "TypeError: First argument to String.prototype.{name} must not be a regular expression"
        );
        Ok(to_string(heap, value)?.chars().collect())
    };
    let find_from = |needle: &[char], from: usize| -> Option<usize> {
        if needle.is_empty() {
            return Some(from.min(len));
        }
        (from..len).find(|&i| chars[i..].starts_with(needle))
    };
    Ok(match name {
        "at" => {
            let relative = integer_or_infinity(to_number(heap, &arg(args, 0))?);
            let index = if relative < 0.0 { len as f64 + relative } else { relative };
            if index < 0.0 || index >= len as f64 {
                VmValue::Undefined
            } else {
                VmValue::String(chars[index as usize].to_string())
            }
        }
        "charAt" | "charCodeAt" | "codePointAt" => {
            let index = integer_or_infinity(to_number(heap, &arg(args, 0))?);
            let found = (index >= 0.0 && index < len as f64).then(|| chars[index as usize]);
            match (name, found) {
                ("charAt", Some(c)) => VmValue::String(c.to_string()),
                ("charAt", None) => VmValue::String(String::new()),
                ("charCodeAt", Some(c)) => {
                    let mut units = [0u16; 2];
                    number(c.encode_utf16(&mut units)[0] as f64)
                }
                ("charCodeAt", None) => number(f64::NAN),
                (_, Some(c)) => number(c as u32 as f64),
                (_, None) => VmValue::Undefined,
            }
        }
        "concat" => {
            let mut out = text.to_owned();
            for value in args {
                out.push_str(&to_string(heap, value)?);
            }
            text_result(out)?
        }
        "includes" | "startsWith" => {
            let needle = search_string(heap, &arg(args, 0))?;
            let start = position(heap, &arg(args, 1), 0)?;
            VmValue::Boolean(if name == "startsWith" {
                chars[start..].starts_with(&needle)
            } else {
                find_from(&needle, start).is_some()
            })
        }
        "endsWith" => {
            let needle = search_string(heap, &arg(args, 0))?;
            let end = position(heap, &arg(args, 1), len)?;
            VmValue::Boolean(chars[..end].ends_with(&needle))
        }
        "indexOf" => {
            let needle: Vec<char> = to_string(heap, &arg(args, 0))?.chars().collect();
            let start = position(heap, &arg(args, 1), 0)?;
            number(find_from(&needle, start).map(|i| i as f64).unwrap_or(-1.0))
        }
        "lastIndexOf" => {
            let needle: Vec<char> = to_string(heap, &arg(args, 0))?.chars().collect();
            let from = match arg(args, 1) {
                VmValue::Undefined => f64::INFINITY,
                value => {
                    let n = to_number(heap, &value)?;
                    if n.is_nan() { f64::INFINITY } else { n.trunc() }
                }
            };
            let start = from.clamp(0.0, len as f64) as usize;
            let last = start.min(len.saturating_sub(needle.len()));
            let found = if needle.len() > len {
                None
            } else {
                (0..=last).rev().find(|&i| chars[i..].starts_with(&needle))
            };
            number(found.map(|i| i as f64).unwrap_or(-1.0))
        }
        "padStart" | "padEnd" => {
            let target = to_number(heap, &arg(args, 0))?;
            let target = if target.is_nan() { 0.0 } else { target.trunc() };
            let fill = match arg(args, 1) {
                VmValue::Undefined => " ".to_owned(),
                value => to_string(heap, &value)?,
            };
            if target <= len as f64 || fill.is_empty() {
                VmValue::String(text.to_owned())
            } else {
                ensure!(target <= TEXT_RESULT_LIMIT as f64, "RangeError: Invalid string length");
                let needed = target as usize - len;
                let padding: String = fill.chars().cycle().take(needed).collect();
                text_result(if name == "padStart" {
                    format!("{padding}{text}")
                } else {
                    format!("{text}{padding}")
                })?
            }
        }
        "repeat" => {
            let count = integer_or_infinity(to_number(heap, &arg(args, 0))?);
            ensure!(count >= 0.0 && count.is_finite(), "RangeError: Invalid count value: {count}");
            ensure!(
                (text.len() as f64) * count <= TEXT_RESULT_LIMIT as f64,
                "RangeError: Invalid string length"
            );
            VmValue::String(text.repeat(count as usize))
        }
        "replace" | "replaceAll" => replace(heap, text, name, args)?,
        "search" => {
            let value=arg(args,0);
            let (re,flags)=regex_argument(heap,&value)?;
            let id=regex_id(heap,&value)?;
            let saved=heap.regex_last_index(id);
            // Search starts at zero and restores lastIndex, including on failure.
            heap.set_property(id,"lastIndex".into(),number(0.0))?;
            let found=regex_exec(heap,id,&re,&flags,text);
            heap.set_property(id,"lastIndex".into(),saved)?;
            number(found?.and_then(|c|c.get(0)).map(|m|char_index(text,m.start()) as f64).unwrap_or(-1.0))
        }
        "match" => {
            let value=arg(args,0);
            let (re,flags)=regex_argument(heap,&value)?;
            let id=regex_id(heap,&value)?;
            if flags.contains('g') {
                let matches=regex_collect(heap,id,&re,&flags,text,true,true)?.iter().map(|c|VmValue::String(c.get(0).unwrap().as_str().into())).collect::<Vec<_>>();
                if matches.is_empty(){VmValue::Null}else{new_array(heap,matches)?}
            } else {
                match regex_exec(heap,id,&re,&flags,text)? {Some(c)=>match_array(heap,&re,&c,text)?,None=>VmValue::Null}
            }
        }
        "matchAll" => {
            let value=arg(args,0);
            let original=regex_parts(heap,&value);
            ensure!(original.as_ref().is_none_or(|(_,f)|f.contains('g')),"TypeError: String.prototype.matchAll called with a non-global RegExp argument");
            let (source,flags)=if let Some(parts)=original {parts}else{(to_string(heap,&value)?,"g".into())};
            let re=super::values::compile_regex(&source,&flags)?;
            let VmValue::Reference(id)=construct_regexp(heap,&[VmValue::String(source),VmValue::String(flags.clone())])? else {unreachable!()};
            if let VmValue::Reference(original)=value && heap.regex_parts(original).is_some() {heap.set_property(id,"lastIndex".into(),heap.regex_last_index(original))?;}
            let mut all=Vec::new();
            for captures in regex_collect(heap,id,&re,&flags,text,true,false)? {all.push(match_array(heap,&re,&captures,text)?);}
            new_array(heap,all)?
        }
        "slice" => {
            let start = match arg(args, 0) {
                VmValue::Undefined => 0,
                value => relative_position(to_number(heap, &value)?, len),
            };
            let end = match arg(args, 1) {
                VmValue::Undefined => len,
                value => relative_position(to_number(heap, &value)?, len),
            };
            VmValue::String(chars[start..end.max(start)].iter().collect())
        }
        "substring" => {
            let start = position(heap, &arg(args, 0), 0)?;
            let end = position(heap, &arg(args, 1), len)?;
            let (from, to) = (start.min(end), start.max(end));
            VmValue::String(chars[from..to].iter().collect())
        }
        "substr" => {
            let start = match arg(args, 0) {
                VmValue::Undefined => 0,
                value => relative_position(to_number(heap, &value)?, len),
            };
            let count = match arg(args, 1) {
                VmValue::Undefined => len - start,
                value => integer_or_infinity(to_number(heap, &value)?).clamp(0.0, (len - start) as f64) as usize,
            };
            VmValue::String(chars[start..start + count].iter().collect())
        }
        "split" => split(heap, text, args)?,
        "toLowerCase" | "toLocaleLowerCase" => VmValue::String(text.to_lowercase()),
        "toUpperCase" | "toLocaleUpperCase" => VmValue::String(text.to_uppercase()),
        "toString" | "valueOf" => VmValue::String(text.to_owned()),
        "trim" => VmValue::String(text.trim_matches(is_js_whitespace).to_owned()),
        "trimStart" => VmValue::String(text.trim_start_matches(is_js_whitespace).to_owned()),
        "trimEnd" => VmValue::String(text.trim_end_matches(is_js_whitespace).to_owned()),
        _ => bail!("TypeError: string.{name} is not a function"),
    })
}

/// The default sort order: undefined last, otherwise by string value in
/// UTF-16 code unit order. The sort is stable, as the specification requires.
fn default_sort(heap: &Heap, items: Vec<VmValue>) -> Result<Vec<VmValue>> {
    let mut keyed = Vec::with_capacity(items.len());
    let mut undefined = 0;
    for item in items {
        if matches!(item, VmValue::Undefined) {
            undefined += 1;
        } else {
            let key: Vec<u16> = to_string(heap, &item)?.encode_utf16().collect();
            keyed.push((key, item));
        }
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let mut sorted: Vec<VmValue> = keyed.into_iter().map(|(_, item)| item).collect();
    sorted.extend(std::iter::repeat_n(VmValue::Undefined, undefined));
    Ok(sorted)
}

/// Array.prototype methods that take no callback. Callback methods are
/// compiled into the calling frame so that callbacks can suspend.
fn array_method(heap: &mut Heap, id: HeapId, name: &str, args: &[VmValue]) -> Result<VmValue> {
    if matches!(name,"keys"|"values"|"entries") {let kind=match name{"keys"=>super::js_collections::Iteration::Keys,"entries"=>super::js_collections::Iteration::Entries,_=>super::js_collections::Iteration::Values};return super::js_collections::iterator(heap,VmValue::Reference(id),Some(kind),false);}
    let items = array_items(heap, id)?;
    let holes = heap.holes(id);
    let len = items.len();
    let present = |index: usize| !holes.contains(&index);
    Ok(match name {
        "at" => {
            let relative = integer_or_infinity(to_number(heap, &arg(args, 0))?);
            let index = if relative < 0.0 { len as f64 + relative } else { relative };
            if index < 0.0 || index >= len as f64 || !present(index as usize) {
                VmValue::Undefined
            } else {
                items[index as usize].clone()
            }
        }
        "concat" => {
            let mut out = items.clone();
            let mut out_holes: BTreeSet<usize> = holes.clone();
            for value in args {
                let spread = if let VmValue::Reference(other) = value {
                    let key = heap.well_known_symbol("isConcatSpreadable");
                    let explicit = key.as_ref().and_then(|key| super::vm_eval::member(heap, value, key).ok())
                        .filter(|value| !matches!(value, VmValue::Undefined));
                    explicit.map(|value| super::vm_eval::truthy(&value))
                        .or_else(|| matches!(heap.get(*other), Ok(HeapNode::Array(_))).then_some(true))
                        .unwrap_or(false)
                } else { false };
                match value {
                    VmValue::Reference(other) if spread && matches!(heap.get(*other)?, HeapNode::Array(_)) => {
                        let offset = out.len();
                        let other_holes = heap.holes(*other);
                        out.extend(array_items(heap, *other)?);
                        out_holes.extend(other_holes.into_iter().map(|i| i + offset));
                    }
                    other => out.push(other.clone()),
                }
            }
            ensure!(out.len() <= ARRAY_LIMIT, "RangeError: array exceeds the runtime limit");
            let result = heap.allocate(HeapNode::Array(Vec::new()))?;
            heap.replace_array(result, out, out_holes)?;
            VmValue::Reference(result)
        }
        "includes" | "indexOf" => {
            let needle = arg(args, 0);
            let start = match arg(args, 1) {
                VmValue::Undefined => 0,
                value => relative_position(to_number(heap, &value)?, len),
            };
            let found = (start..len).find(|&i| {
                if name == "includes" {
                    let value = if present(i) { &items[i] } else { &VmValue::Undefined };
                    same_value_zero(value, &needle)
                } else {
                    present(i) && strict_equal(&items[i], &needle)
                }
            });
            if name == "includes" {
                VmValue::Boolean(found.is_some())
            } else {
                number(found.map(|i| i as f64).unwrap_or(-1.0))
            }
        }
        "lastIndexOf" => {
            let needle = arg(args, 0);
            if len == 0 {
                return Ok(number(-1.0));
            }
            let from = match arg(args, 1) {
                VmValue::Undefined => len as f64 - 1.0,
                value => integer_or_infinity(to_number(heap, &value)?),
            };
            let start = if from < 0.0 { len as f64 + from } else { from.min(len as f64 - 1.0) };
            if start < 0.0 {
                number(-1.0)
            } else {
                let found = (0..=start as usize).rev().find(|&i| present(i) && strict_equal(&items[i], &needle));
                number(found.map(|i| i as f64).unwrap_or(-1.0))
            }
        }
        "join" | "toString" => {
            let separator = match (name, arg(args, 0)) {
                ("toString", _) | (_, VmValue::Undefined) => ",".to_owned(),
                (_, value) => to_string(heap, &value)?,
            };
            let mut parts = Vec::with_capacity(len);
            for (index, item) in items.iter().enumerate() {
                parts.push(if !present(index) || matches!(item, VmValue::Undefined | VmValue::Null) {
                    String::new()
                } else {
                    to_string(heap, item)?
                });
            }
            text_result(parts.join(&separator))?
        }
        "slice" => {
            let start = match arg(args, 0) {
                VmValue::Undefined => 0,
                value => relative_position(to_number(heap, &value)?, len),
            };
            let end = match arg(args, 1) {
                VmValue::Undefined => len,
                value => relative_position(to_number(heap, &value)?, len),
            };
            let end = end.max(start);
            let result = heap.allocate(HeapNode::Array(Vec::new()))?;
            let slice_holes = holes.iter().filter(|i| **i >= start && **i < end).map(|i| i - start).collect();
            heap.replace_array(result, items[start..end].to_vec(), slice_holes)?;
            VmValue::Reference(result)
        }
        "push" => {
            ensure!(len + args.len() <= ARRAY_LIMIT, "RangeError: array exceeds the runtime limit");
            number(heap.append(id, args.to_vec())? as f64)
        }
        "pop" | "shift" => {
            if len == 0 {
                return Ok(VmValue::Undefined);
            }
            let index = if name == "pop" { len - 1 } else { 0 };
            let removed = if present(index) { items[index].clone() } else { VmValue::Undefined };
            let mut rest = items;
            rest.remove(index);
            let rest_holes = holes
                .iter()
                .filter(|i| **i != index)
                .map(|i| if name == "shift" { i - 1 } else { *i })
                .collect();
            heap.replace_array(id, rest, rest_holes)?;
            removed
        }
        "unshift" => {
            ensure!(len + args.len() <= ARRAY_LIMIT, "RangeError: array exceeds the runtime limit");
            let mut out = args.to_vec();
            let shifted = holes.iter().map(|i| i + args.len()).collect();
            out.extend(items);
            let length = out.len();
            heap.replace_array(id, out, shifted)?;
            number(length as f64)
        }
        "splice" => {
            if args.is_empty() {
                return new_array(heap, Vec::new());
            }
            let start = relative_position(to_number(heap, &args[0])?, len);
            let delete = if args.len() == 1 {
                len - start
            } else {
                integer_or_infinity(to_number(heap, &args[1])?).clamp(0.0, (len - start) as f64) as usize
            };
            let inserted = args.get(2..).unwrap_or(&[]).to_vec();
            ensure!(len - delete + inserted.len() <= ARRAY_LIMIT, "RangeError: array exceeds the runtime limit");
            let removed: Vec<VmValue> = items[start..start + delete].to_vec();
            let removed_holes = holes.iter().filter(|i| **i >= start && **i < start + delete).map(|i| i - start).collect();
            let mut out = items[..start].to_vec();
            out.extend(inserted.iter().cloned());
            out.extend(items[start + delete..].iter().cloned());
            let shift = inserted.len() as isize - delete as isize;
            let out_holes = holes
                .iter()
                .filter(|i| **i < start || **i >= start + delete)
                .map(|i| if *i < start { *i } else { (*i as isize + shift) as usize })
                .collect();
            heap.replace_array(id, out, out_holes)?;
            let result = heap.allocate(HeapNode::Array(Vec::new()))?;
            heap.replace_array(result, removed, removed_holes)?;
            VmValue::Reference(result)
        }
        "reverse" | "toReversed" => {
            let mut out = items;
            out.reverse();
            let mirrored: BTreeSet<usize> = holes.iter().map(|i| len - 1 - i).collect();
            if name == "reverse" {
                heap.replace_array(id, out, mirrored)?;
                VmValue::Reference(id)
            } else {
                let out = out
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| if mirrored.contains(&i) { VmValue::Undefined } else { v })
                    .collect();
                new_array(heap, out)?
            }
        }
        "fill" => {
            let value = arg(args, 0);
            let start = match arg(args, 1) {
                VmValue::Undefined => 0,
                v => relative_position(to_number(heap, &v)?, len),
            };
            let end = match arg(args, 2) {
                VmValue::Undefined => len,
                v => relative_position(to_number(heap, &v)?, len),
            };
            let mut out = items;
            for item in out.iter_mut().take(end).skip(start) {
                *item = value.clone();
            }
            let kept = holes.iter().filter(|i| **i < start || **i >= end).copied().collect();
            heap.replace_array(id, out, kept)?;
            VmValue::Reference(id)
        }
        "flat" => {
            let depth = match arg(args, 0) {
                VmValue::Undefined => 1.0,
                value => integer_or_infinity(to_number(heap, &value)?),
            };
            fn flatten(heap: &Heap, id: HeapId, depth: f64, out: &mut Vec<VmValue>) -> Result<()> {
                let holes = heap.holes(id);
                for (index, item) in array_items(heap, id)?.into_iter().enumerate() {
                    if holes.contains(&index) {
                        continue;
                    }
                    match item {
                        VmValue::Reference(inner) if depth >= 1.0 && matches!(heap.get(inner)?, HeapNode::Array(_)) => {
                            flatten(heap, inner, depth - 1.0, out)?
                        }
                        other => out.push(other),
                    }
                    ensure!(out.len() <= ARRAY_LIMIT, "RangeError: array exceeds the runtime limit");
                }
                Ok(())
            }
            let mut out = Vec::new();
            flatten(heap, id, depth, &mut out)?;
            new_array(heap, out)?
        }
        "sort" | "toSorted" => {
            ensure!(
                matches!(arg(args, 0), VmValue::Undefined),
                "TypeError: comparator functions are compiled at the call site; pass an arrow function directly"
            );
            let values: Vec<VmValue> = items
                .iter()
                .enumerate()
                .filter(|(i, _)| present(*i))
                .map(|(_, v)| v.clone())
                .collect();
            let present_count = values.len();
            let sorted = default_sort(heap, values)?;
            if name == "sort" {
                let mut out = sorted;
                out.resize(len, VmValue::Undefined);
                let tail = (present_count..len).collect();
                heap.replace_array(id, out, tail)?;
                VmValue::Reference(id)
            } else {
                let mut out = sorted;
                out.resize(len, VmValue::Undefined);
                new_array(heap, out)?
            }
        }
        _ => bail!("TypeError: array.{name} is not a function"),
    })
}

/// Number.prototype.toFixed: the exact binary value rounded half up, as the
/// specification's "larger n" rule requires.
fn to_fixed(value: f64, digits: usize) -> String {
    if !value.is_finite() || value.abs() >= 1e21 {
        return number_to_string(value);
    }
    let negative = value < 0.0;
    // A double has at most 1074 fractional digits, so this expansion is exact.
    let exact = format!("{:.1100}", value.abs());
    let (whole, fraction) = exact.split_once('.').unwrap();
    let mut digits_out: Vec<u8> = whole.bytes().chain(fraction.bytes().take(digits)).map(|b| b - b'0').collect();
    let round_up = fraction.as_bytes().get(digits).is_some_and(|b| *b >= b'5');
    if round_up {
        let mut i = digits_out.len();
        loop {
            if i == 0 {
                digits_out.insert(0, 1);
                break;
            }
            i -= 1;
            if digits_out[i] == 9 {
                digits_out[i] = 0;
            } else {
                digits_out[i] += 1;
                break;
            }
        }
    }
    let text: String = digits_out.iter().map(|d| (b'0' + d) as char).collect();
    let split = text.len() - digits;
    let body = if digits == 0 {
        text
    } else {
        format!("{}.{}", &text[..split], &text[split..])
    };
    let is_zero = body.chars().all(|c| c == '0' || c == '.');
    if negative && !is_zero { format!("-{body}") } else { body }
}

fn radix_string(value: f64, radix: u32) -> String {
    if radix == 10 || !value.is_finite() {
        return number_to_string(value);
    }
    let negative = value < 0.0;
    let mut integer = value.abs().trunc();
    let mut fraction = value.abs() - integer;
    let mut digits = Vec::new();
    if integer == 0.0 {
        digits.push('0');
    }
    while integer >= 1.0 {
        let digit = (integer % radix as f64) as u32;
        digits.push(std::char::from_digit(digit, radix).unwrap());
        integer = (integer / radix as f64).trunc();
    }
    digits.reverse();
    let mut out: String = digits.into_iter().collect();
    if fraction > 0.0 {
        out.push('.');
        for _ in 0..52 {
            fraction *= radix as f64;
            let digit = fraction.trunc() as u32;
            out.push(std::char::from_digit(digit, radix).unwrap());
            fraction -= digit as f64;
            if fraction == 0.0 {
                break;
            }
        }
    }
    if negative { format!("-{out}") } else { out }
}

/// Number.prototype.toLocaleString for the en/zh grouping this host uses:
/// comma groups of three and at most three fraction digits, half expanded.
fn locale_string(value: f64) -> String {
    if !value.is_finite() {
        return if value.is_nan() { "NaN".into() } else if value > 0.0 { "∞".into() } else { "-∞".into() };
    }
    let fixed = to_fixed(value, 3);
    let (whole, fraction) = fixed.split_once('.').unwrap_or((&fixed, ""));
    let (sign, whole) = whole.strip_prefix('-').map(|w| ("-", w)).unwrap_or(("", whole));
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{fraction}")
    }
}

/// Exact decimal rounding of the binary number, choosing the larger
/// significand at ties. Rust precision formatting uses a different tie rule.
fn to_exponential(value:f64, digits:Option<usize>)->String {
    if !value.is_finite(){return number_to_string(value);}
    let negative=value<0.0;
    let abs=value.abs();
    if digits.is_none(){
        let text=format!("{abs:e}");let (mantissa,exponent)=text.split_once('e').unwrap();
        let exponent:i32=exponent.parse().unwrap();return format!("{}{mantissa}e{exponent:+}",if negative{"-"}else{""});
    }
    let count=digits.unwrap()+1;
    let exact=format!("{abs:.1100}");let (whole,fraction)=exact.split_once('.').unwrap();
    let all:Vec<u8>=whole.bytes().chain(fraction.bytes()).map(|b|b-b'0').collect();
    let first=all.iter().position(|n|*n!=0);
    let mut exponent=first.map(|i|whole.len() as i32-i as i32-1).unwrap_or(0);
    let mut out=if let Some(first)=first{all[first..].iter().copied().take(count).collect::<Vec<_>>()}else{vec![]};
    out.resize(count,0);
    if first.is_some_and(|first|all.get(first+count).is_some_and(|digit|*digit>=5)){
        let mut i=count;loop{if i==0{out.insert(0,1);out.pop();exponent+=1;break;}i-=1;if out[i]==9{out[i]=0;}else{out[i]+=1;break;}}
    }
    let mut text=String::new();if negative{text.push('-');}text.push((b'0'+out[0])as char);
    if count>1{text.push('.');text.extend(out[1..].iter().map(|n|(b'0'+*n)as char));}
    format!("{text}e{exponent:+}")
}

fn number_method(heap: &mut Heap, value: f64, name: &str, args: &[VmValue]) -> Result<VmValue> {
    Ok(match name {
        "toExponential"=>{
            let argument=arg(args,0);
            let digits=integer_or_infinity(to_number(heap,&argument)?);
            if !value.is_finite(){return Ok(VmValue::String(number_to_string(value)));}
            ensure!((0.0..=100.0).contains(&digits),"RangeError: Number.prototype.toExponential fractionDigits must be between 0 and 100");
            VmValue::String(to_exponential(value,if argument==VmValue::Undefined{None}else{Some(digits as usize)}))
        },
        "toFixed" => {
            let digits = integer_or_infinity(to_number(heap, &arg(args, 0))?);
            ensure!((0.0..=100.0).contains(&digits), "RangeError: toFixed() digits argument must be between 0 and 100");
            VmValue::String(to_fixed(value, digits as usize))
        }
        "toString" => {
            let radix = match arg(args, 0) {
                VmValue::Undefined => 10.0,
                radix => integer_or_infinity(to_number(heap, &radix)?),
            };
            ensure!((2.0..=36.0).contains(&radix), "RangeError: toString() radix must be between 2 and 36");
            VmValue::String(radix_string(value, radix as u32))
        }
        "toLocaleString" => {
            if let VmValue::Reference(id)=arg(args,1) { ensure!(heap.own_names(id)?.is_empty(),"RangeError: Number.toLocaleString options are unsupported"); }
            match arg(args, 0) {
                VmValue::Undefined => {}
                locale => {
                    let locale = to_string(heap, &locale)?;
                    let parsed=locale.parse::<icu_locale::Locale>().map_err(|e|anyhow::anyhow!("RangeError: invalid number locale: {e}"))?;
                    ensure!(
                        matches!(parsed.id.language.as_str(),"en"|"zh") && parsed.extensions.is_empty(),
                        "RangeError: locale {locale} is not available in the flow runtime"
                    );
                }
            }
            VmValue::String(locale_string(value))
        }
        "valueOf" => number(value),
        _ => bail!("TypeError: number.{name} is not a function"),
    })
}

fn regex_method(heap: &mut Heap, id: HeapId, pattern: &str, flags: &str, name: &str, args: &[VmValue]) -> Result<VmValue> {
    let re = super::values::compile_regex(pattern, flags)?;
    if name == "toString" {
        return Ok(VmValue::String(format!("/{pattern}/{flags}")));
    }
    let input = to_string(heap, &arg(args, 0))?;
    ensure!(input.len() <= 2 * 1024 * 1024, "RangeError: regex input exceeds 2 MiB");
    let captures=regex_exec(heap,id,&re,flags,&input)?;
    Ok(match name {
        "test" => VmValue::Boolean(captures.is_some()),
        "exec" => match captures {
            Some(captures) => match_array(heap, &re, &captures, &input)?,
            None => VmValue::Null,
        },
        _ => bail!("TypeError: regexp.{name} is not a function"),
    })
}

/// Call `receiver.name(...args)` for a built-in receiver kind.
pub(super) fn call_method(heap: &mut Heap, receiver: VmValue, name: &str, args: &[VmValue]) -> Result<VmValue> {
    if name=="hasOwnProperty" {
        let key=arg(args,0);
        return builtin(heap,"Object.hasOwn",&[receiver,key]);
    }
    if name=="valueOf" && matches!(&receiver,VmValue::Reference(id) if heap.date_value(*id).is_none()) {return Ok(receiver);}
    if matches!(&receiver,VmValue::Reference(id) if matches!(heap.get(*id),Ok(HeapNode::Collection(_) | HeapNode::Iterator(_)))) {
        return super::js_collections::call(heap,receiver,name,args);
    }
    match receiver {
        VmValue::String(text) => string_method(heap, &text, name, args),
        VmValue::Utf16(units) => {
            match name {
                "valueOf" | "toString" => Ok(VmValue::Utf16(units)),
                "charCodeAt" | "codePointAt" | "charAt" | "at" => {
                    let index = integer_or_infinity(to_number(heap, &arg(args,0))?);
                    let index = if name == "at" && index < 0.0 { units.len() as f64 + index } else { index };
                    let value = if index >= 0.0 && index < units.len() as f64 { units.get(index as usize).copied() } else { None };
                    Ok(match (name, value) {
                        ("charCodeAt" | "codePointAt", Some(unit)) => number(unit as f64),
                        ("charCodeAt", None) => number(f64::NAN),
                        ("charAt", None) => VmValue::String(String::new()),
                        (_, Some(unit)) => super::js_value::from_utf16(vec![unit]),
                        _ => VmValue::Undefined,
                    })
                }
                _ => bail!("TypeError: {name} on an ill-formed UTF-16 string is unavailable; preserve it with JSON.stringify"),
            }
        }
        VmValue::Number(value) => number_method(heap, value.get(), name, args),
        VmValue::Boolean(value) => match name {
            "toString" => Ok(VmValue::String(value.to_string())),
            "valueOf" => Ok(VmValue::Boolean(value)),
            _ => bail!("TypeError: boolean.{name} is not a function"),
        },
        VmValue::Reference(id) => {
            if heap.date_value(id).is_some() { return super::js_date::method(heap, id, name, args); }
            if matches!(heap.get(id)?, HeapNode::Array(_)) {
                return array_method(heap, id, name, args);
            }
            if let Some((pattern, flags)) = heap.regex_parts(id) {
                return regex_method(heap, id, &pattern, &flags, name, args);
            }
            match name {
                "toString" => Ok(VmValue::String(to_string(heap, &receiver)?)),
                "hasOwnProperty" => {
                    let key = to_string(heap, &arg(args, 0))?;
                    Ok(VmValue::Boolean(match heap.get(id)? {
                        HeapNode::Object(properties) => properties.iter().any(|(k, _)| *k == key),
                        _ => false,
                    }))
                }
                _ => bail!("TypeError: object.{name} is not a function"),
            }
        }
        VmValue::Undefined | VmValue::Null => {
            bail!("TypeError: Cannot read properties of {} (reading '{name}')", if matches!(receiver, VmValue::Null) { "null" } else { "undefined" })
        }
        VmValue::Symbol(_) => bail!("TypeError: incompatible Symbol method {name}"),
        VmValue::Promise(_) => bail!("TypeError: promise.{name} is not available"),
    }
}

// ---------------------------------------------------------------- globals

fn parse_int(text: &str, radix: f64) -> f64 {
    let mut s = text.trim_start_matches(is_js_whitespace);
    let mut sign = 1.0;
    if let Some(rest) = s.strip_prefix('-') {
        sign = -1.0;
        s = rest;
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest;
    }
    let mut radix = radix as u32;
    let mut strip_prefix = true;
    if radix != 0 {
        if !(2..=36).contains(&radix) {
            return f64::NAN;
        }
        if radix != 16 {
            strip_prefix = false;
        }
    } else {
        radix = 10;
    }
    if strip_prefix && (s.starts_with("0x") || s.starts_with("0X")) {
        s = &s[2..];
        radix = 16;
    }
    let digits: String = s.chars().take_while(|c| c.to_digit(radix).is_some()).collect();
    if digits.is_empty() {
        return f64::NAN;
    }
    let mut value = 0.0f64;
    for c in digits.chars() {
        value = value * radix as f64 + c.to_digit(radix).unwrap() as f64;
    }
    sign * value
}

fn parse_float(text: &str) -> f64 {
    let s = text.trim_start_matches(is_js_whitespace);
    for prefix in ["Infinity", "+Infinity", "-Infinity"] {
        if s.starts_with(prefix) {
            return if prefix.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
        }
    }
    // The longest prefix that is a StrDecimalLiteral.
    let bytes = s.as_bytes();
    let mut best = None;
    let mut i = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        i = 1;
    }
    let mut digits = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if digits > 0 {
        best = Some(i);
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
        if digits > 0 {
            best = Some(i);
        }
    }
    if digits > 0 && i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < bytes.len() && matches!(bytes[j], b'+' | b'-') {
            j += 1;
        }
        let start = j;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > start {
            best = Some(j);
        }
    }
    match best {
        Some(end) => string_to_number(&s[..end]),
        None => f64::NAN,
    }
}

/// Own enumerable string keys in OrdinaryOwnPropertyKeys order: array
/// indices ascending, then the rest in insertion order.
fn own_keys(heap: &Heap, value: &VmValue) -> Result<Vec<(String, VmValue)>> {
    if heap.has_realm() && let VmValue::Reference(id)=value {
        return heap.own_names(*id)?.into_iter().filter(|key|!Heap::is_symbol_key(key)&&heap.enumerable(*id,key)).map(|key|Ok((match heap.key_value(&key){VmValue::String(key)=>key,_=>unreachable!()},heap.own_property(*id,&key)?.unwrap_or(VmValue::Undefined)))).collect();
    }
    Ok(match value {
        VmValue::Reference(id) => match heap.get(*id)? {
            HeapNode::Object(properties) => {
                let index = |key: &str| key.parse::<u32>().ok().filter(|n| *n != u32::MAX && n.to_string() == key);
                let mut ordered: Vec<_> = properties.iter().cloned().collect();
                ordered.sort_by_key(|(key, _)| {
                    let i = index(key);
                    (i.is_none(), i.unwrap_or(0))
                });
                ordered
            }
            HeapNode::Array(items) => {
                let holes = heap.holes(*id);
                let mut properties: Vec<_> = items
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !holes.contains(i))
                    .map(|(i, v)| (i.to_string(), v.clone()))
                    .collect();
                properties.extend_from_slice(heap.array_properties(*id));
                properties
            }
            HeapNode::Collection(_) | HeapNode::Iterator(_) | HeapNode::Closure{..} | HeapNode::Native(_) | HeapNode::BoundFunction{..} | HeapNode::PromiseResolver{..}=>{
                let mut properties=heap.array_properties(*id).to_vec();
                properties.sort_by_key(|(key,_)|{let index=key.parse::<u32>().ok().filter(|n|*n!=u32::MAX && n.to_string()==*key);(index.is_none(),index.unwrap_or(0))});
                properties
            },
            _ => Vec::new(),
        },
        VmValue::String(text) => text.chars().enumerate().map(|(i, c)| (i.to_string(), VmValue::String(c.to_string()))).collect(),
        VmValue::Undefined | VmValue::Null => bail!("TypeError: Cannot convert undefined or null to object"),
        _ => Vec::new(),
    })
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JSON.stringify with an optional key list replacer and indentation.
fn json_stringify(heap: &Heap, value: &VmValue, keys: Option<&[String]>, gap: &str) -> Result<Option<String>> {
    fn serialize(
        heap: &Heap,
        value: &VmValue,
        keys: Option<&[String]>,
        gap: &str,
        indent: &str,
        stack: &mut Vec<HeapId>,
    ) -> Result<Option<String>> {
        Ok(Some(match value {
            VmValue::Null => "null".into(),
            VmValue::Boolean(v) => v.to_string(),
            VmValue::Number(n) => {
                let n = n.get();
                if n.is_finite() { number_to_string(n) } else { "null".into() }
            }
            VmValue::String(s) => quote(s),
            VmValue::Utf16(units) => {
                let mut text = String::from("\"");
                for decoded in char::decode_utf16(units.iter().copied()) {
                    match decoded {
                        Ok(c) => { let quoted=quote(&c.to_string());text.push_str(&quoted[1..quoted.len()-1]); }
                        Err(error) => text.push_str(&format!("\\u{:04x}",error.unpaired_surrogate())),
                    }
                }
                text.push('"'); text
            },
            VmValue::Undefined | VmValue::Symbol(_) => return Ok(None),
            VmValue::Promise(_) => "{}".into(),
            VmValue::Reference(id) => {
                let node = heap.get(*id)?;
                if let Some(value)=heap.boxed_value(*id){return serialize(heap,value,keys,gap,indent,stack);}
                if let Some(ms) = heap.date_value(*id) {
                    return Ok(Some(if ms.is_finite() { quote(&super::js_date::iso(ms)?) } else { "null".into() }));
                }
                if matches!(node, HeapNode::Closure { .. } | HeapNode::BoundFunction { .. } | HeapNode::Native(_) | HeapNode::PromiseResolver { .. }) {
                    return Ok(None);
                }
                ensure!(!stack.contains(id), "TypeError: Converting circular structure to JSON");
                stack.push(*id);
                let inner = format!("{indent}{gap}");
                let result = match node {
                    HeapNode::Array(items) => {
                        let holes = heap.holes(*id);
                        let mut parts = Vec::with_capacity(items.len());
                        for (index, item) in items.iter().enumerate() {
                            let inherited=if holes.contains(&index){heap.lookup_property(*id,&index.to_string())?}else{None};
                            let item=if holes.contains(&index){inherited.as_ref().unwrap_or(&VmValue::Undefined)}else{item};
                            parts.push(serialize(heap, item, keys, gap, &inner, stack)?.unwrap_or_else(|| "null".into()));
                        }
                        if parts.is_empty() {
                            "[]".into()
                        } else if gap.is_empty() {
                            format!("[{}]", parts.join(","))
                        } else {
                            format!("[\n{inner}{}\n{indent}]", parts.join(&format!(",\n{inner}")))
                        }
                    }
                    _ => {
                        let mut properties = own_keys(heap, value)?;
                        if let Some(keys) = keys {
                            properties = keys
                                .iter()
                                .filter_map(|key| properties.iter().find(|(k, _)| k == key).cloned())
                                .collect();
                        }
                        let mut parts = Vec::new();
                        for (key, item) in properties {
                            if let Some(text) = serialize(heap, &item, keys, gap, &inner, stack)? {
                                let separator = if gap.is_empty() { ":" } else { ": " };
                                parts.push(format!("{}{separator}{text}", quote(&key)));
                            }
                        }
                        if parts.is_empty() {
                            "{}".into()
                        } else if gap.is_empty() {
                            format!("{{{}}}", parts.join(","))
                        } else {
                            format!("{{\n{inner}{}\n{indent}}}", parts.join(&format!(",\n{inner}")))
                        }
                    }
                };
                stack.pop();
                result
            }
        }))
    }
    let text = serialize(heap, value, keys, gap, "", &mut Vec::new())?;
    if let Some(text) = &text {
        ensure!(text.len() <= TEXT_RESULT_LIMIT, "RangeError: JSON.stringify result exceeds 4 MiB");
    }
    Ok(text)
}

/// Global functions and static methods, by their dotted name.
pub(super) fn builtin_arity(name: &str) -> Option<(usize, usize)> {
    if super::js_collections::NAMES.contains(&name) || name.strip_prefix("new ").is_some_and(|n|super::js_collections::NAMES.contains(&n)) {return Some((0,64));}
    if matches!(name,"$$iterator"|"$$arrayFromIterator"|"$$collectionEntries"|"$$iteratorValue"|"$$iterableArray") {return Some((1,1));}
    Some(match name {
        "$$rawEncodeURI"|"$$rawDecodeURI"|"$$rawEncodeURIComponent"|"$$rawDecodeURIComponent"=>(1,1),"String.raw"=>(1,64),
        "$$functionName"=>(3,3),"$$rawExponential"=>(2,2),
        "$$rawLocaleCompare"|"$$rawDateLocale"=>(4,4),"$$rawNumberLocale"=>(3,3),
        "$$privateKey"=>(1,1),"$$privateAdd"|"$$privateMethod"=>(5,5),"$$privateHas"|"$$classInitializer"=>(2,2),
        "$$classSetup"|"$$classMethod"=>(5,5),"$$methodHome"|"$$classBinding"|"$$boxPrimitive"=>(2,2),"$$superBase"|"$$isDerived"=>(1,1),"$$superCallArray"=>(1,2),"$$superConstructor"=>(0,0),"$$constructReady"=>(4,4),
        "$$rawLooseEqual"|"$$objectRest"=>(2,2),
        "$$builtinStringIterator"|"$$rawDateConstruct"|"$$rawNumber"|"$$rawString"|"$$rawSymbol"|"$$rawSymbolFor"|"$$isDate"=>(1,1),
        "$$awaitSubscribe"=>(3,3),
        "$$getAsyncIterator"|"$$getIterator"|"$$iteratorStep"|"$$iteratorCloseNormal"=>(1,1),"$$iteratorCloseThrow"=>(2,2),
        "Symbol"=>(0,1),"Symbol.for"|"Symbol.keyFor"=>(1,1),
        "Object.defineProperty"|"Reflect.defineProperty" => (3,3),
        "Object.defineProperties"|"Object.getOwnPropertyDescriptor"|"Reflect.getOwnPropertyDescriptor" =>(2,2),
        "Object.getOwnPropertyDescriptors"|"Object.getOwnPropertyNames"|"Object.getOwnPropertySymbols"|"Object.preventExtensions"|"Object.isExtensible"|"Object.seal"|"Object.isSealed"|"Object.isFrozen"|"Reflect.ownKeys"|"Reflect.isExtensible"|"Reflect.preventExtensions"|"Reflect.getPrototypeOf"=>(1,1),
        "Reflect.has"|"Reflect.deleteProperty"|"Reflect.setPrototypeOf"=>(2,2),
        "Reflect.get"=>(2,3),"Reflect.set"=>(3,4),"Reflect.apply"=>(3,3),"Reflect.construct"=>(2,3),
        "$$keyOperation"=>(3,3),
        "$$makeBound"=>(5,5),
        "$$defineOwn"|"$$createData"=>(3,3),"$$literalAccessor"|"$$prepareSet"=>(4,4),"$$getProperty"=>(2,3),"$$constructWithTarget"|"$$invokeWithReceiver"=>(3,3),
        "$$emptyCollection"|"$$defaultTag"|"$$iteratorPlan"|"$$arrayLikeOnly"|"$$boundTarget"|"$$propertyKey"|"$$requireCoercible"|"$$createObject"|"$$jsonUnbox"|"$$isPromise"|"$$isObject"|"$$isConstructor"|"$$keys"=>(1,1),"$$rawStringify"=>(1,3),
        "Math.abs" | "Math.floor" | "Math.ceil" | "Math.round" | "Math.trunc" | "Math.sign"
        | "Math.sqrt" | "Math.cbrt" | "Math.exp" | "Math.log" | "Math.log2" | "Math.log10"
        | "Math.sin" | "Math.cos" | "Math.tan" | "Math.asin" | "Math.acos" | "Math.atan" => (1, 1),
        "Math.pow" | "Math.atan2" => (2, 2),
        "Math.max" | "Math.min" | "Math.hypot" => (0, 64),
        "Math.random" => (0, 0),
        "Number" | "String" | "Boolean" => (0, 1),
        "parseInt" | "Number.parseInt" => (1, 2),
        "parseFloat" | "Number.parseFloat" | "isNaN" | "isFinite" | "Number.isInteger"
        | "Number.isSafeInteger" | "Number.isFinite" | "Number.isNaN" => (1, 1),
        "Object.keys" | "Object.values" | "Object.entries" | "Object.fromEntries" | "Array.isArray"
        | "Array.from" => (1, 1),
        "Object"=>(0,1),"Function"=>(0,64),"Object.create"=>(1,2),"Object.getPrototypeOf"|"$$forInKeys"=>(1,1),"Object.setPrototypeOf"=>(2,2),
        "Object.assign" => (1, 64),
        "Array.of" => (0, 64),
        "Array" | "new Array" | "String.fromCharCode" | "String.fromCodePoint" => (0, 64),
        "Date" | "new Date" | "Date.UTC" => (0, 64),
        "Date.now" => (0, 0),
        "Date.parse" | "encodeURI" | "decodeURI" | "encodeURIComponent" | "decodeURIComponent" | "structuredClone"
        | "Object.freeze" => (1, 1),
        "Object.hasOwn" | "Object.is" => (2, 2),
        "JSON.stringify" => (1, 3),
        "JSON.parse" => (1, 1),
        "$$hasPrototype" | "delete" | "$$has" | "$$sortWriteBack" => (2, 2),
        "$$replacementMatches" => (3, 3),
        "$$textLimit" => (1, 1),
        _ if name.strip_prefix("new ").is_some_and(|class| super::vm_eval::ERROR_CLASSES.contains(&class)) => (0, 2),
        _ => return None,
    })
}

/// Static constants such as Math.PI, as source text for a numeric literal.
pub(super) fn constant(name: &str) -> Option<f64> {
    Some(match name {
        "Math.PI" => std::f64::consts::PI,
        "Math.E" => std::f64::consts::E,
        "Math.LN2" => std::f64::consts::LN_2,
        "Math.LN10" => std::f64::consts::LN_10,
        "Math.LOG2E" => std::f64::consts::LOG2_E,
        "Math.LOG10E" => std::f64::consts::LOG10_E,
        "Math.SQRT2" => std::f64::consts::SQRT_2,
        "Math.SQRT1_2" => std::f64::consts::FRAC_1_SQRT_2,
        "Number.MAX_SAFE_INTEGER" => 9007199254740991.0,
        "Number.MIN_SAFE_INTEGER" => -9007199254740991.0,
        "Number.EPSILON" => f64::EPSILON,
        "Number.MAX_VALUE" => f64::MAX,
        "Number.MIN_VALUE" => 5e-324,
        "Number.POSITIVE_INFINITY" | "Infinity" => f64::INFINITY,
        "Number.NEGATIVE_INFINITY" => f64::NEG_INFINITY,
        "Number.NaN" | "NaN" => f64::NAN,
        _ => return None,
    })
}

fn math(name: &str, values: &[f64]) -> f64 {
    let x = values.first().copied().unwrap_or(f64::NAN);
    match name {
        "Math.abs" => x.abs(),
        "Math.floor" => x.floor(),
        "Math.ceil" => x.ceil(),
        "Math.round" => {
            if !x.is_finite() || x == 0.0 {
                x
            } else {
                let floor = x.floor();
                if x - floor >= 0.5 { floor + 1.0 } else { floor }
            }
        }
        "Math.trunc" => x.trunc(),
        "Math.sign" => {
            if x.is_nan() || x == 0.0 { x } else { x.signum() }
        }
        "Math.sqrt" => x.sqrt(),
        "Math.cbrt" => x.cbrt(),
        "Math.exp" => x.exp(),
        "Math.log" => x.ln(),
        "Math.log2" => x.log2(),
        "Math.log10" => x.log10(),
        "Math.sin" => x.sin(),
        "Math.cos" => x.cos(),
        "Math.tan" => x.tan(),
        "Math.asin" => x.asin(),
        "Math.acos" => x.acos(),
        "Math.atan" => x.atan(),
        "Math.atan2" => x.atan2(values.get(1).copied().unwrap_or(f64::NAN)),
        "Math.pow" => js_pow(x, values.get(1).copied().unwrap_or(f64::NAN)),
        "Math.max" => values.iter().fold(f64::NEG_INFINITY, |a, b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(*b) }),
        "Math.min" => values.iter().fold(f64::INFINITY, |a, b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(*b) }),
        "Math.hypot" => values.iter().map(|v| v * v).sum::<f64>().sqrt(),
        _ => f64::NAN,
    }
}

/// Number::exponentiate, which differs from powf for |base| = 1 and an
/// infinite or NaN exponent.
pub(super) fn js_pow(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() || (base.abs() == 1.0 && exponent.is_infinite()) {
        f64::NAN
    } else {
        base.powf(exponent)
    }
}

pub(super) fn builtin(heap: &mut Heap, name: &str, args: &[VmValue]) -> Result<VmValue> {
    if name=="$$rawExponential" {let VmValue::Number(value)=args[0] else{bail!("TypeError: Number.prototype.toExponential requires a Number receiver")};return number_method(heap,value.get(),"toExponential",&args[1..]);}
    if let Some(name)=match name {"$$rawEncodeURI"=>Some("encodeURI"),"$$rawDecodeURI"=>Some("decodeURI"),"$$rawEncodeURIComponent"=>Some("encodeURIComponent"),"$$rawDecodeURIComponent"=>Some("decodeURIComponent"),_=>None} { return builtin(heap,name,args); }
    if name=="$$rawLocaleCompare" { return super::js_locale::compare(heap,args); }
    if name=="$$rawNumberLocale" { return number_method(heap,to_number(heap,&arg(args,0))?,"toLocaleString",&args[1..]); }
    if name=="$$rawDateLocale" { return super::js_date::locale(heap,args); }
    if let Some(value)=heap.class_builtin(name,args)?{return Ok(value);}
    if let Some(value)=heap.symbol_builtin(name,args)?{return Ok(value);}
    if let Some(value)=heap.descriptor_builtin(name,args)?{return Ok(value);}
    if let Some(value)=heap.prototype_builtin(name,args)?{return Ok(value);}
    if name=="RegExp" {
        if let Some(VmValue::Reference(id))=args.first() && heap.regex_parts(*id).is_some() && args.get(1).is_none_or(|v|*v==VmValue::Undefined){return Ok(VmValue::Reference(*id));}
        return construct_regexp(heap,args);
    }
    if super::vm_eval::ERROR_CLASSES.contains(&name){return new_error(heap,name,&arg(args,0),&arg(args,1));}
    if name=="$$iteratorValue" {
        let VmValue::Reference(id)=arg(args,0) else {bail!("TypeError: iterator receiver is not an object")};
        return Ok(super::js_collections::next(heap,id)?.unwrap_or(VmValue::Undefined));
    }
    if name=="$$iterableArray" {let values=super::js_collections::materialize(heap,arg(args,0),false)?;return new_array(heap,values);}
    if let Some(kind)=name.strip_prefix("new ").filter(|n|super::js_collections::NAMES.contains(n)) {return super::js_collections::construct(heap,kind,args);}
    if super::js_collections::NAMES.contains(&name) {bail!("TypeError: {name} constructor requires new");}
    if name.contains(".prototype.") && (super::js_collections::NAMES.iter().any(|kind|name.starts_with(kind)) || name.contains(" Iterator.prototype.")) {bail!("TypeError: unbound collection method requires a compatible receiver");}
    if matches!(name,"$$iterator"|"$$arrayFromIterator"|"$$collectionEntries") {
        return super::js_collections::iterator(heap,arg(args,0),if name=="$$collectionEntries" {Some(super::js_collections::Iteration::Entries)}else{None},name=="$$arrayFromIterator");
    }
    #[cfg(test)]
    if name=="test262.error" {return new_error(heap,"Test262Error",&arg(args,0),&arg(args,1));}
    let first = arg(args, 0);
    if name == "new Date" { return super::js_date::construct(heap, args); }
    if name == "Date" || name.starts_with("Date.") { return super::js_date::static_call(heap, name, args); }
    if name == "new Array" { return builtin(heap, "Array", args); }
    if let Some(class) = name.strip_prefix("new ") {
        ensure!(super::vm_eval::ERROR_CLASSES.contains(&class), "TypeError: {class} is not a constructor");
        return new_error(heap, class, &first, &arg(args, 1));
    }
    Ok(match name {
        "$$textLimit" => text_result(to_string(heap, &first)?)?,
        "$$replacementMatches" => {
            let text = to_string(heap, &first)?;
            let pattern = arg(args, 1);
            let all = matches!(arg(args, 2), VmValue::Boolean(true));
            let mut matches: Vec<(usize, usize, Vec<VmValue>, Option<Vec<(String, VmValue)>>)> = Vec::new();
            let mut captured_bytes = 0usize;
            if let Some((source, flags)) = regex_parts(heap, &pattern) {
                ensure!(!all || flags.contains('g'), "TypeError: replaceAll must be called with a global RegExp");
                ensure!(text.len() <= 2 * 1024 * 1024, "RangeError: regex input exceeds 2 MiB");
                let re = super::values::compile_regex(&source, &flags)?;
                let id=regex_id(heap,&pattern)?;
                let global=flags.contains('g');
                for captures in regex_collect(heap,id,&re,&flags,&text,global,global)? {
                    ensure!(matches.len() < REGEX_RESULT_LIMIT, "RangeError: too many replacements");
                    captured_bytes += captures.iter().flatten().map(|m| m.as_str().len()).sum::<usize>();
                    ensure!(captured_bytes <= TEXT_RESULT_LIMIT, "RangeError: replacement matches exceed 4 MiB");
                    let whole = captures.get(0).unwrap();
                    let capture_args = captures.iter().map(|m| match m { Some(m) => VmValue::String(m.as_str().to_owned()), None => VmValue::Undefined }).collect();
                    let names: Vec<_> = re.capture_names().enumerate().filter_map(|(index, name)| name.map(|name| (name.to_owned(), captures.get(index).map(|m| VmValue::String(m.as_str().to_owned())).unwrap_or(VmValue::Undefined)))).collect();
                    matches.push((whole.start(), whole.end(), capture_args, (!names.is_empty()).then_some(names)));
                    if !flags.contains('g') { break; }
                }
            } else {
                let search = to_string(heap, &pattern)?;
                let positions: Vec<usize> = if !all { text.find(&search).into_iter().collect() }
                    else if search.is_empty() { std::iter::once(0).chain(text.char_indices().map(|(i, c)| i + c.len_utf8())).collect() }
                    else { text.match_indices(&search).map(|(i, _)| i).collect() };
                ensure!(positions.len() <= REGEX_RESULT_LIMIT, "RangeError: too many replacements");
                ensure!(positions.len().saturating_mul(search.len()) <= TEXT_RESULT_LIMIT, "RangeError: replacement matches exceed 4 MiB");
                for start in positions { matches.push((start, start + search.len(), vec![VmValue::String(search.clone())], None)); }
            }
            let mut rows = Vec::new();
            for (start, end, callback_args, groups) in matches {
                let start = char_index(&text, start);
                let end = char_index(&text, end);
                let has_groups = groups.is_some();
                let groups = match groups { Some(groups) => VmValue::Reference(heap.allocate(HeapNode::Object(groups))?), None => VmValue::Undefined };
                let callback_args = new_array(heap, callback_args)?;
                rows.push(VmValue::Reference(heap.allocate(HeapNode::Object(vec![("start".into(), number(start as f64)), ("end".into(), number(end as f64)), ("args".into(), callback_args), ("hasGroups".into(), VmValue::Boolean(has_groups)), ("groups".into(), groups)]))?));
            }
            new_array(heap, rows)?
        }
        "$$has" => {
            let VmValue::Reference(id) = first else { bail!("TypeError: receiver is not an array") };
            let index = to_number(heap, &arg(args, 1))?;
            VmValue::Boolean(index >= 0.0 && index.fract() == 0.0 && heap.has_property(id,&(index as usize).to_string())?)
        }
        "$sortWriteBack" | "$$sortWriteBack" => {
            // sort() in place: the sorted values, then the original holes.
            let (VmValue::Reference(target), VmValue::Reference(sorted)) = (&first, &arg(args, 1)) else {
                bail!("internal sort write-back needs arrays")
            };
            let original = array_items(heap, *target)?.len();
            let mut items = array_items(heap, *sorted)?;
            let count = items.len();
            items.resize(original.max(count), VmValue::Undefined);
            heap.replace_array(*target, items, (count..original).collect())?;
            VmValue::Reference(*target)
        }
        "delete" => {
            if let VmValue::Reference(id) = first {
                let key = heap.property_key(&arg(args, 1))?;
                heap.delete_property(id, &key)?;
            }
            VmValue::Boolean(true)
        }
        _ if name.starts_with("Math.") => {
            if name == "Math.random" {
                return Ok(number(rand_unit()));
            }
            let mut values = Vec::with_capacity(args.len());
            for value in args {
                values.push(to_number(heap, value)?);
            }
            number(math(name, &values))
        }
        "Number" => number(if args.is_empty() { 0.0 } else { to_number(heap, &first)? }),
        "String" if matches!(first,VmValue::Symbol(_))=>{let VmValue::Symbol(id)=first else{unreachable!()};heap.symbol_text(id)?},
        "String" => if matches!(first, VmValue::Utf16(_)) { first } else { VmValue::String(if args.is_empty() { String::new() } else { to_string(heap, &first)? }) },
        "Boolean" => VmValue::Boolean(super::vm_eval::truthy(&first)),
        "Array" => {
            if args.len() == 1 && matches!(first, VmValue::Number(_)) {
                let length = to_number(heap, &first)?;
                ensure!(length.is_finite() && length >= 0.0 && length.fract() == 0.0 && length <= ARRAY_LIMIT as f64, "RangeError: Invalid array length (runtime limit 1000000)");
                let value = new_array(heap, vec![VmValue::Undefined; length as usize])?;
                let VmValue::Reference(id) = value else { unreachable!() };
                for i in 0..length as usize { heap.set_hole(id, i); }
                value
            } else { new_array(heap, args.to_vec())? }
        }
        "String.fromCharCode" | "String.fromCodePoint" => {
            let mut units = Vec::new();
            for value in args {
                let n = to_number(heap, value)?;
                if name == "String.fromCharCode" {
                    units.push(if n.is_finite() { n.trunc().rem_euclid(65536.0) as u16 } else { 0 });
                } else {
                    ensure!(n.is_finite() && n.fract() == 0.0 && (0.0..=0x10ffff as f64).contains(&n), "RangeError: Invalid code point");
                    let code = n as u32;
                    if code <= 0xffff { units.push(code as u16); }
                    else { units.push(0xd800 + ((code - 0x10000) >> 10) as u16); units.push(0xdc00 + ((code - 0x10000) & 0x3ff) as u16); }
                }
            }
            // Scalar strings never silently replace an unpaired surrogate.
            super::js_value::from_utf16(units)
        }
        "encodeURI" | "encodeURIComponent" => {
            if matches!(first, VmValue::Utf16(_)) { bail!("URIError: URI malformed (unpaired UTF-16 surrogate)"); }
            let text = to_string(heap, &first)?;
            let mut out = String::new();
            for byte in text.bytes() {
                if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) || name == "encodeURI" && b";/?:@&=+$,#".contains(&byte) { out.push(byte as char); }
                else { out.push_str(&format!("%{byte:02X}")); }
                ensure!(out.len() <= TEXT_RESULT_LIMIT, "RangeError: URI result exceeds 4 MiB");
            }
            VmValue::String(out)
        }
        "decodeURI" | "decodeURIComponent" => {
            let units: Vec<u16> = match &first {
                VmValue::Utf16(units) => units.clone(),
                value => to_string(heap, value)?.encode_utf16().collect(),
            };
            let mut out: Vec<u16> = Vec::new();
            let mut i = 0;
            while i < units.len() {
                if units[i] == b'%' as u16 {
                    let mut encoded = Vec::new();
                    while units.get(i) == Some(&(b'%' as u16)) {
                        let hi = units.get(i+1).and_then(|u| char::from_u32(*u as u32)).and_then(|c| c.to_digit(16));
                        let lo = units.get(i+2).and_then(|u| char::from_u32(*u as u32)).and_then(|c| c.to_digit(16));
                        let (Some(hi),Some(lo)) = (hi,lo) else { bail!("URIError: URI malformed"); };
                        encoded.push((hi*16+lo) as u8); i+=3;
                    }
                    let decoded=std::str::from_utf8(&encoded).map_err(|_| anyhow::anyhow!("URIError: URI malformed"))?;
                    let mut offset = i - encoded.len() * 3;
                    for ch in decoded.chars() {
                        if name == "decodeURI" && ch.is_ascii() && b";/?:@&=+$,#".contains(&(ch as u8)) { out.extend_from_slice(&units[offset..offset+3]); }
                        else { out.extend(ch.encode_utf16(&mut [0;2]).iter().copied()); }
                        offset += ch.len_utf8() * 3;
                    }
                } else { out.push(units[i]); i += 1; }
                ensure!(out.len() <= TEXT_RESULT_LIMIT, "RangeError: URI result exceeds runtime limit");
            }
            super::js_value::from_utf16(out)
        }
        "structuredClone" => {
            heap.ensure_json_cloneable(&first,&mut Default::default())?;
            let json = heap.to_json(&first)?;
            heap.from_json(&json)?
        }
        "Object.freeze" => {
            if let VmValue::Reference(id) = first { heap.freeze(id); }
            first
        }
        "Object.is" => {
            let other = arg(args, 1);
            VmValue::Boolean(match (&first, &other) {
                (VmValue::Number(a), VmValue::Number(b)) => (a.get().is_nan() && b.get().is_nan()) || a.get() == b.get() && a.get().is_sign_negative() == b.get().is_sign_negative(),
                _ => strict_equal(&first, &other),
            })
        }
        "Object.hasOwn" => {
            ensure!(!matches!(first, VmValue::Null | VmValue::Undefined), "TypeError: Cannot convert undefined or null to object");
            let key = heap.property_key(&arg(args, 1))?;
            VmValue::Boolean(match &first {
                VmValue::Reference(id) => heap.own_property(*id, &key)?.is_some(),
                VmValue::String(text) => key == "length" || key.parse::<usize>().ok().is_some_and(|i| i.to_string() == key && i < text.chars().count()),
                _ => false,
            })
        }
        "parseInt" | "Number.parseInt" => {
            let text = to_string(heap, &first)?;
            let radix = match arg(args, 1) {
                VmValue::Undefined => 0.0,
                value => {
                    let n = to_number(heap, &value)?;
                    if n.is_finite() { n.trunc() } else { 0.0 }
                }
            };
            number(parse_int(&text, radix))
        }
        "parseFloat" | "Number.parseFloat" => number(parse_float(&to_string(heap, &first)?)),
        "isNaN" => VmValue::Boolean(to_number(heap, &first)?.is_nan()),
        "isFinite" => VmValue::Boolean(to_number(heap, &first)?.is_finite()),
        "Number.isNaN" => VmValue::Boolean(matches!(first, VmValue::Number(n) if n.get().is_nan())),
        "Number.isFinite" => VmValue::Boolean(matches!(first, VmValue::Number(n) if n.get().is_finite())),
        "Number.isInteger" => VmValue::Boolean(matches!(first, VmValue::Number(n) if n.get().is_finite() && n.get().trunc() == n.get())),
        "Number.isSafeInteger" => VmValue::Boolean(
            matches!(first, VmValue::Number(n) if n.get().is_finite() && n.get().trunc() == n.get() && n.get().abs() <= 9007199254740991.0),
        ),
        "Array.isArray" => VmValue::Boolean(matches!(&first, VmValue::Reference(id) if matches!(heap.get(*id), Ok(HeapNode::Array(_))))),
        "Array.of" => new_array(heap, args.to_vec())?,
        "Array.from" => {let values=super::js_collections::materialize(heap,first,true)?;new_array(heap,values)?},
        "Object.keys" | "Object.values" | "Object.entries" => {
            let properties = own_keys(heap, &first)?;
            let mut out = Vec::with_capacity(properties.len());
            for (key, value) in properties {
                out.push(match name {
                    "Object.keys" => VmValue::String(key),
                    "Object.values" => value,
                    _ => new_array(heap, vec![VmValue::String(key), value])?,
                });
            }
            new_array(heap, out)?
        }
        "Object.fromEntries" => {
            let entries=super::js_collections::materialize(heap,first,false)?;
            let mut properties: Vec<(String, VmValue)> = Vec::new();
            for entry in entries {
                let VmValue::Reference(_) = &entry else {
                    bail!("TypeError: Iterator value is not an entry object")
                };
                let property=super::vm_eval::member(heap,&entry,&VmValue::String("0".into()))?;
                let key=to_string(heap,&property)?;
                let value=super::vm_eval::member(heap,&entry,&VmValue::String("1".into()))?;
                if let Some(existing) = properties.iter_mut().find(|(k, _)| *k == key) {
                    existing.1 = value;
                } else {
                    properties.push((key, value));
                }
            }
            VmValue::Reference(heap.allocate(HeapNode::Object(properties))?)
        }
        "Object.assign" => {
            let VmValue::Reference(target) = first else {
                bail!("TypeError: Object.assign target must be an object")
            };
            for source in &args[1..] {
                if matches!(source, VmValue::Undefined | VmValue::Null) {
                    continue;
                }
                for (key, value) in own_keys(heap, source)? {
                    heap.set_property(target, key, value)?;
                }
            }
            VmValue::Reference(target)
        }
        "JSON.stringify" => {
            let keys = match arg(args, 1) {
                VmValue::Reference(id) if matches!(heap.get(id)?, HeapNode::Array(_)) => {
                    let mut keys = Vec::new();
                    for key in array_items(heap, id)? {
                        if matches!(key, VmValue::String(_) | VmValue::Number(_)) {
                            let key = to_string(heap, &key)?;
                            if !keys.contains(&key) {
                                keys.push(key);
                            }
                        }
                    }
                    Some(keys)
                }
                ref value if super::vm_eval::callable(heap,value) => {
                    bail!("TypeError: JSON.stringify replacer functions are not available yet")
                }
                _ => None,
            };
            let gap = match arg(args, 2) {
                VmValue::Number(n) => " ".repeat(integer_or_infinity(n.get()).clamp(0.0, 10.0) as usize),
                VmValue::String(s) => s.chars().take(10).collect(),
                _ => String::new(),
            };
            match json_stringify(heap, &first, keys.as_deref(), &gap)? {
                Some(text) => VmValue::String(text),
                None => VmValue::Undefined,
            }
        }
        "JSON.parse" => {
            let text = to_string(heap, &first)?;
            let parsed: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("SyntaxError: {e}"))?;
            heap.from_json(&parsed)?
        }
        _ => {
            // Only the explicitly JSON-based library crosses this boundary.
            // An unknown native must name itself before inspecting arguments.
            ensure!(super::builtins::arity(name).is_some(),"TypeError: {name} is unavailable in the flow runtime; consult the program guide for supported functions");
            let mut values = Vec::with_capacity(args.len());
            for value in args {
                values.push(heap.to_json(value)?);
            }
            // The built-ins name themselves in their errors.
            let result = super::builtins::call(name, values)?;
            heap.from_json(&result)?
        }
    })
}

pub(super) fn construct_regexp(heap:&mut Heap,args:&[VmValue])->Result<VmValue>{
    let first=arg(args,0);
    let existing=if let VmValue::Reference(id)=first{heap.regex_parts(id)}else{None};
    let pattern=if let Some((pattern,_))=&existing{pattern.clone()}else if first==VmValue::Undefined{String::new()}else{to_string(heap,&first)?};
    let flags=if arg(args,1)==VmValue::Undefined{existing.map(|(_,flags)|flags).unwrap_or_default()}else{to_string(heap,&arg(args,1))?};
    let value=super::values::regexp(vec![serde_json::json!(pattern),serde_json::json!(flags)])?;
    heap.from_json(&value)
}

/// `new Error(message, {cause})`: name and message are stored as properties
/// so that they also reach AI tasks and tools across the JSON boundary.
pub(super) fn new_error(heap: &mut Heap, class: &str, message: &VmValue, options: &VmValue) -> Result<VmValue> {
    let message = match message {
        VmValue::Undefined => String::new(),
        value => to_string(heap, value)?,
    };
    let mut properties = vec![
        ("name".to_owned(), VmValue::String(class.to_owned())),
        ("message".to_owned(), VmValue::String(message)),
    ];
    if let VmValue::Reference(id) = options
        && matches!(heap.get(*id)?, HeapNode::Object(_))
    {
        let cause = heap.property(*id, "cause")?;
        if !matches!(cause, VmValue::Undefined) {
            properties.push(("cause".to_owned(), cause));
        }
    }
    let id = heap.allocate(HeapNode::Object(properties))?;
    heap.set_class(id, class);
    Ok(VmValue::Reference(id))
}

/// Math.random from the operating system's randomness; flow state is saved
/// after each step, so the value never needs to be reproduced.
fn rand_unit() -> f64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        bytes = hasher.finish().to_le_bytes();
    }
    (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_rounds_the_exact_binary_value_half_up() {
        // Expected strings come from Node.js 24.
        for (value, digits, expected) in [
            (1.005, 2, "1.00"),
            (2.5, 0, "3"),
            (0.5, 0, "1"),
            (-1.5, 0, "-2"),
            (1234.5678, 1, "1234.6"),
            (1e21, 2, "1e+21"),
            (0.000001, 7, "0.0000010"),
            (-0.0001, 2, "0.00"),
        ] {
            assert_eq!(to_fixed(value, digits), expected, "{value}.toFixed({digits})");
        }
    }

    #[test]
    fn substitution_patterns_match_javascript() {
        let text = "aXbXc";
        assert_eq!(substitution("[$&]", text, 1, 2, &[], &[]), "[X]");
        assert_eq!(substitution("$$", text, 1, 2, &[], &[]), "$");
        assert_eq!(substitution("$1", "x", 0, 1, &[], &[]), "$1");
        assert_eq!(substitution("<$1>", "a1", 1, 2, &[Some("1".into())], &[]), "<1>");
        assert_eq!(substitution("$`|$'", text, 1, 2, &[], &[]), "a|bXc");
    }

    #[test]
    fn number_parsing_globals_match_javascript() {
        assert_eq!(parse_int("  42px", 0.0), 42.0);
        assert_eq!(parse_int("0x1F", 0.0), 31.0);
        assert_eq!(parse_int("1F", 16.0), 31.0);
        assert_eq!(parse_int("z", 36.0), 35.0);
        assert!(parse_int("", 0.0).is_nan());
        assert_eq!(parse_int("-12.9", 0.0), -12.0);
        assert_eq!(parse_float("3.14abc"), 3.14);
        assert_eq!(parse_float(".5e2x"), 50.0);
        assert_eq!(parse_float("Infinityx"), f64::INFINITY);
        assert_eq!(locale_string(1234567.891), "1,234,567.891");
        assert_eq!(locale_string(0.1), "0.1");
        assert_eq!(radix_string(255.0, 16), "ff");
        assert_eq!(radix_string(255.0, 2), "11111111");
    }
}
