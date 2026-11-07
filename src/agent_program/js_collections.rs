//! Identity-bearing collections and live built-in iterators. No JSON roundtrip.
use super::heap::{Heap, HeapId, HeapNode, JsNumber, VmValue};
use super::{js_value as js, vm_eval};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub(super) const NAMES: &[&str] = &["Map", "Set", "WeakMap", "WeakSet"];
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Kind {
    Map,
    Set,
    WeakMap,
    WeakSet,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Map => "Map",
            Self::Set => "Set",
            Self::WeakMap => "WeakMap",
            Self::WeakSet => "WeakSet",
        }
    }
    pub fn weak(self) -> bool {
        matches!(self, Self::WeakMap | Self::WeakSet)
    }
    pub fn map(self) -> bool {
        matches!(self, Self::Map | Self::WeakMap)
    }
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "Map" => Self::Map,
            "Set" => Self::Set,
            "WeakMap" => Self::WeakMap,
            "WeakSet" => Self::WeakSet,
            _ => return None,
        })
    }
    pub fn method(self, name: &str) -> bool {
        matches!(name, "has" | "delete")
            || if self.map() {
                matches!(name, "get" | "set")
            } else {
                name == "add"
            }
            || (!self.weak() && matches!(name, "clear" | "keys" | "values" | "entries" | "forEach"))
    }
    fn method_name(self, name: &str) -> String {
        let name = if self == Self::Set && name == "keys" {
            "values"
        } else {
            name
        };
        format!("{}.prototype.{name}", self.name())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Symbol(u64),
    Undefined,
    Null,
    Boolean(bool),
    Number(u64),
    String(Vec<u16>),
    Reference(HeapId),
    LegacyPromise(String),
}
fn key(value: &VmValue) -> Key {
    match value {
        VmValue::Symbol(id)=>Key::Symbol(*id),
        VmValue::Undefined => Key::Undefined,
        VmValue::Null => Key::Null,
        VmValue::Boolean(v) => Key::Boolean(*v),
        VmValue::Number(v) => {
            let n = v.get();
            Key::Number(if n == 0.0 {
                0
            } else if n.is_nan() {
                f64::NAN.to_bits()
            } else {
                n.to_bits()
            })
        }
        VmValue::String(s) => Key::String(s.encode_utf16().collect()),
        VmValue::Utf16(s) => Key::String(s.clone()),
        VmValue::Reference(id) => Key::Reference(*id),
        VmValue::Promise(id) => Key::LegacyPromise(format!("{id:?}")),
    }
}
fn canonical(value: VmValue) -> VmValue {
    if matches!(&value,VmValue::Number(n) if n.get()==0.0) {
        VmValue::Number(JsNumber::new(0.0))
    } else {
        value
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Collection {
    pub kind: Kind,
    pub entries: Vec<Option<(VmValue, VmValue)>>,
    pub size: usize,
    // A cache, rebuilt lazily after restoring. Tombstones must survive: live
    // iterators retain their positions even across clear/delete/reinsert.
    #[serde(skip)]
    index: Option<HashMap<Key, usize>>,
}
impl PartialEq for Collection {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.entries == other.entries && self.size == other.size
    }
}
impl Collection {
    fn index(&mut self) -> &mut HashMap<Key, usize> {
        self.index.get_or_insert_with(|| {
            self.entries
                .iter()
                .enumerate()
                .filter_map(|(i, e)| e.as_ref().map(|(k, _)| (key(k), i)))
                .collect()
        })
    }
    fn find(&mut self, k: &VmValue) -> Option<usize> {
        self.index().get(&key(k)).copied()
    }
    fn insert(&mut self, k: VmValue, v: VmValue) {
        let k = canonical(k);
        let v = if self.kind.map() { v } else { k.clone() };
        if let Some(i) = self.find(&k) {
            self.entries[i] = Some((k, v));
        } else {
            let i = self.entries.len();
            self.index().insert(key(&k), i);
            self.entries.push(Some((k, v)));
            self.size += 1;
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Iteration {
    Keys,
    Values,
    Entries,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct IteratorState {
    pub source: Option<VmValue>,
    pub cursor: usize,
    pub kind: Iteration,
    pub tag: String,
    pub limit: Option<usize>,
}
fn array(heap: &mut Heap, values: Vec<VmValue>) -> Result<VmValue> {
    Ok(VmValue::Reference(heap.allocate(HeapNode::Array(values))?))
}
fn intern(heap: &mut Heap, kind: Kind) -> Result<()> {
    heap.native(kind.name())?;
    for name in [
        "get", "set", "add", "has", "delete", "clear", "keys", "values", "entries", "forEach",
    ] {
        if kind.method(name) {
            heap.native(&kind.method_name(name))?;
        }
    }
    Ok(())
}
pub(super) fn member(heap: &Heap, id: HeapId, name: &str) -> Result<VmValue> {
    if let Some(value) = heap.own_property(id, name)? {
        return Ok(value);
    }
    Ok(match heap.get(id)? {
        HeapNode::Collection(c) => {
            if name == "size" && !c.kind.weak() {
                VmValue::Number(JsNumber::new(c.size as f64))
            } else if name == "constructor" {
                heap.native_reference(c.kind.name())
                    .unwrap_or(VmValue::Undefined)
            } else if c.kind.method(name) {
                heap.native_reference(&c.kind.method_name(name))
                    .unwrap_or(VmValue::Undefined)
            } else {
                VmValue::Undefined
            }
        }
        HeapNode::Iterator(it) if name == "next" => heap
            .native_reference(&format!("{}.prototype.next", it.tag))
            .unwrap_or(VmValue::Undefined),
        _ => VmValue::Undefined,
    })
}
pub(super) fn iterator(
    heap: &mut Heap,
    value: VmValue,
    kind: Option<Iteration>,
    array_like: bool,
) -> Result<VmValue> {
    if let VmValue::Reference(id) = &value {
        if matches!(heap.get(*id)?, HeapNode::Iterator(_)) {
            return Ok(value);
        }
    }
    let mut source = value;
    let mut limit = None;
    let (tag, default) = match &source {
        VmValue::Reference(id) => match heap.get(*id)? {
            HeapNode::Collection(c) if !c.kind.weak() => (
                format!("{} Iterator", c.kind.name()),
                if c.kind.map() {
                    Iteration::Entries
                } else {
                    Iteration::Values
                },
            ),
            HeapNode::Array(_) => ("Array Iterator".into(), Iteration::Values),
            _ if array_like => {
                let length = vm_eval::member(heap, &source, &VmValue::String("length".into()))?;
                let n = js::to_number(heap, &length)?;
                let n = if n.is_nan() || n <= 0.0 {
                    0.0
                } else {
                    n.floor()
                };
                ensure!(
                    n <= 1_000_000.0,
                    "RangeError: Array.from length exceeds runtime limit 1000000"
                );
                limit = Some(n as usize);
                ("ArrayLike Iterator".into(), Iteration::Values)
            }
            _ => bail!("TypeError: value is not a supported built-in iterable"),
        },
        VmValue::String(_) | VmValue::Utf16(_) => {
            let units = js::string_units(heap, &source)?;
            let mut chars = vec![];
            let mut i = 0;
            while i < units.len() {
                let width = if (0xd800..=0xdbff).contains(&units[i])
                    && units
                        .get(i + 1)
                        .is_some_and(|u| (0xdc00..=0xdfff).contains(u))
                {
                    2
                } else {
                    1
                };
                chars.push(js::from_utf16(units[i..i + width].to_vec()));
                i += width;
            }
            source = array(heap, chars)?;
            ("String Iterator".into(), Iteration::Values)
        }
        VmValue::Null | VmValue::Undefined => {
            bail!("TypeError: value is null or undefined, not iterable")
        }
        _ if array_like => {
            source = array(heap, vec![])?;
            ("ArrayLike Iterator".into(), Iteration::Values)
        }
        _ => bail!("TypeError: value is not a supported built-in iterable"),
    };
    heap.native(&format!("{tag}.prototype.next"))?;
    Ok(VmValue::Reference(heap.allocate(HeapNode::Iterator(
        IteratorState {
            source: Some(source),
            cursor: 0,
            kind: kind.unwrap_or(default),
            tag,
            limit,
        },
    ))?))
}
pub(super) fn next(heap: &mut Heap, id: HeapId) -> Result<Option<VmValue>> {
    let HeapNode::Iterator(state) = heap.get(id)? else {
        bail!("TypeError: iterator.next requires an iterator receiver")
    };
    let (source, mut cursor, kind, limit) =
        (state.source.clone(), state.cursor, state.kind, state.limit);
    let Some(source) = source else {
        return Ok(None);
    };
    let mut pair = None;
    if let VmValue::Reference(source_id) = source {
        if let Some(limit) = limit {
            if cursor < limit {
                let value = vm_eval::member(heap, &source, &VmValue::String(cursor.to_string()))?;
                pair = Some((VmValue::Number(JsNumber::new(cursor as f64)), value));
                cursor += 1;
            }
        } else {
            match heap.get(source_id)? {
                HeapNode::Collection(c) => {
                    while cursor < c.entries.len() {
                        let entry = c.entries[cursor].clone();
                        cursor += 1;
                        if let Some((k, v)) = entry {
                            pair = Some((k, v));
                            break;
                        }
                    }
                }
                HeapNode::Array(items) => {
                    if cursor < items.len() {
                        let value=vm_eval::member(heap,&source,&VmValue::String(cursor.to_string()))?;
                        pair = Some((VmValue::Number(JsNumber::new(cursor as f64)), value));
                        cursor += 1;
                    }
                }
                _ => bail!("TypeError: invalid built-in iterator source"),
            }
        }
    }
    let HeapNode::Iterator(state) = heap.get_mut(id)? else {
        unreachable!()
    };
    state.cursor = cursor;
    let Some((k, v)) = pair else {
        state.source = None;
        return Ok(None);
    };
    Ok(Some(match kind {
        Iteration::Keys => k,
        Iteration::Values => v,
        Iteration::Entries => array(heap, vec![k, v])?,
    }))
}
pub(super) fn materialize(
    heap: &mut Heap,
    value: VmValue,
    array_like: bool,
) -> Result<Vec<VmValue>> {
    let VmValue::Reference(id) = iterator(heap, value, None, array_like)? else {
        unreachable!()
    };
    let mut values = vec![];
    while let Some(value) = next(heap, id)? {
        ensure!(
            values.len() < 1_000_000,
            "RangeError: iterable exceeds runtime array limit 1000000"
        );
        values.push(value);
    }
    Ok(values)
}
pub(super) fn construct(heap: &mut Heap, name: &str, args: &[VmValue]) -> Result<VmValue> {
    let kind = Kind::parse(name).context("unknown collection constructor")?;
    intern(heap, kind)?;
    let id = heap.allocate(HeapNode::Collection(Collection {
        kind,
        entries: vec![],
        size: 0,
        index: Some(HashMap::new()),
    }))?;
    let source = args.first().cloned().unwrap_or(VmValue::Undefined);
    if matches!(source, VmValue::Null | VmValue::Undefined) {
        return Ok(VmValue::Reference(id));
    }
    let VmValue::Reference(it) = iterator(heap, source, None, false)? else {
        unreachable!()
    };
    while let Some(item) = next(heap, it)? {
        let (key, value) = if kind.map() {
            ensure!(
                matches!(item, VmValue::Reference(_)),
                "TypeError: Map constructor entry must be an object"
            );
            (
                vm_eval::member(heap, &item, &VmValue::String("0".into()))?,
                vm_eval::member(heap, &item, &VmValue::String("1".into()))?,
            )
        } else {
            (item.clone(), item)
        };
        ensure!(
            !kind.weak() || matches!(key, VmValue::Reference(_)),
            "TypeError: {} keys must be objects",
            kind.name()
        );
        let HeapNode::Collection(c) = heap.get_mut(id)? else {
            unreachable!()
        };
        c.insert(key, value);
    }
    Ok(VmValue::Reference(id))
}
pub(super) fn call(
    heap: &mut Heap,
    receiver: VmValue,
    name: &str,
    args: &[VmValue],
) -> Result<VmValue> {
    let VmValue::Reference(id) = receiver else {
        bail!("TypeError: collection method requires an object receiver")
    };
    if name == "toString" {
        return Ok(VmValue::String(js::to_string(heap, &receiver)?));
    }
    if name == "valueOf" {
        return Ok(receiver);
    }
    if matches!(heap.get(id)?, HeapNode::Iterator(_)) && name == "next" {
        let value = next(heap, id)?;
        let mut props = vec![
            ("value".into(), value.clone().unwrap_or(VmValue::Undefined)),
            ("done".into(), VmValue::Boolean(value.is_none())),
        ];
        return Ok(VmValue::Reference(
            heap.allocate(HeapNode::Object(std::mem::take(&mut props)))?,
        ));
    }
    let HeapNode::Collection(c) = heap.get(id)? else {
        bail!("TypeError: incompatible collection receiver")
    };
    let kind = c.kind;
    ensure!(
        kind.method(name),
        "TypeError: {}.{name} is not a function",
        kind.name()
    );
    let key = args.first().cloned().unwrap_or(VmValue::Undefined);
    if matches!(name, "keys" | "values" | "entries") {
        let iteration = match name {
            "keys" => Iteration::Keys,
            "entries" => Iteration::Entries,
            _ => Iteration::Values,
        };
        return iterator(heap, receiver, Some(iteration), false);
    }
    let HeapNode::Collection(c) = heap.get_mut(id)? else {
        unreachable!()
    };
    Ok(match name {
        "set" | "add" => {
            ensure!(
                !kind.weak() || matches!(key, VmValue::Reference(_)),
                "TypeError: {} keys must be objects",
                kind.name()
            );
            let value = if kind.map() {
                args.get(1).cloned().unwrap_or(VmValue::Undefined)
            } else {
                canonical(key.clone())
            };
            c.insert(key, value);
            receiver
        }
        "has" => VmValue::Boolean(c.find(&key).is_some()),
        "get" => c
            .find(&key)
            .and_then(|i| c.entries[i].as_ref().map(|(_, v)| v.clone()))
            .unwrap_or(VmValue::Undefined),
        "delete" => {
            let found = c.find(&key);
            if let Some(i) = found {
                c.entries[i] = None;
                c.index().remove(&self::key(&key));
                c.size -= 1;
            }
            VmValue::Boolean(found.is_some())
        }
        "clear" => {
            for entry in &mut c.entries {
                *entry = None;
            }
            c.index = Some(HashMap::new());
            c.size = 0;
            VmValue::Undefined
        }
        _ => bail!(
            "TypeError: {}.{name} requires runtime callback execution",
            kind.name()
        ),
    })
}
