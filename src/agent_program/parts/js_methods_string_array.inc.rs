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

