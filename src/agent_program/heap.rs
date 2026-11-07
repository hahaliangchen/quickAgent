//! Serializable identity-bearing values and lexical environments for the
//! JavaScript-subset interpreter. Closures are not yet connected to execution.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[path="js_prototypes.rs"]
mod prototypes;
#[path="js_descriptors.rs"]
mod descriptors;
use descriptors::PropertyMeta;
#[path="js_symbols.rs"]
mod symbols;
#[path="js_classes.rs"]
mod class_support;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(super) struct HeapId(u64);

/// IEEE-754 bits are stored directly so non-finite values and signed zero
/// survive snapshots independently of JSON's number representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct JsNumber(u64);

impl JsNumber {
    pub(super) fn new(value: f64) -> Self {
        Self(value.to_bits())
    }
    pub(super) fn get(self) -> f64 {
        f64::from_bits(self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum VmValue {
    Undefined,
    Symbol(u64),
    Null,
    Boolean(bool),
    Number(JsNumber),
    String(String),
    /// Only ill-formed UTF-16 strings use this variant. Do not replace their
    /// surrogate code units with U+FFFD while evaluating JavaScript.
    Utf16(Vec<u16>),
    Reference(HeapId),
    Promise(super::promise::PromiseId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum HeapNode {
    Object(#[serde(deserialize_with = "deserialize_properties")] Vec<(String, VmValue)>),
    Array(Vec<VmValue>),
    Collection(super::js_collections::Collection),
    Iterator(super::js_collections::IteratorState),
    Environment {
        parent: Option<HeapId>,
        bindings: BTreeMap<String, VmValue>,
        /// let/const names still in their temporal dead zone.
        #[serde(default)]
        uninitialized: BTreeSet<String>,
    },
    Closure {
        function: String,
        environment: HeapId,
    },
    BoundFunction { target: VmValue, receiver: VmValue, args: Vec<VmValue>, name: String, length: usize },
    /// Promise identity and reactions reside on the serialized heap.
    Promise {
        outcome: Option<super::promise::Outcome>,
        reactions: Vec<super::promise::ContinuationId>,
        handled: bool,
    },
    /// The shared already-resolved flag for a constructor's resolve/reject pair.
    PromiseCapability { promise: HeapId, resolved: bool },
    PromiseResolver { capability: HeapId, reject: bool },
    /// A built-in function or tool used as a value.
    Native(String),
}

fn deserialize_properties<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<(String, VmValue)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Properties {
        Ordered(Vec<(String, VmValue)>),
        Legacy(BTreeMap<String, VmValue>),
    }
    Ok(match Properties::deserialize(deserializer)? {
        Properties::Ordered(values) => values,
        Properties::Legacy(values) => values.into_iter().collect(),
    })
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Heap {
    #[serde(default)]
    class_constructors: BTreeMap<HeapId,bool>,
    #[serde(default)]
    readonly_bindings: BTreeMap<HeapId,BTreeSet<String>>,
    #[serde(default)]
    method_homes: BTreeMap<HeapId,HeapId>,
    next_id: u64,
    nodes: BTreeMap<HeapId, HeapNode>,
    #[serde(default)]
    array_holes: BTreeMap<HeapId, BTreeSet<usize>>,
    /// Objects created by a built-in constructor such as Error keep their
    /// class here, outside their enumerable properties.
    #[serde(default)]
    classes: BTreeMap<HeapId, String>,
    /// One node per built-in function value, so `String === String`.
    #[serde(default)]
    natives: BTreeMap<String, HeapId>,
    #[serde(default)]
    // Also stores collection/iterator own properties; preserve the serialized
    // field name for existing array snapshots.
    array_properties: BTreeMap<HeapId, Vec<(String, VmValue)>>,
    #[serde(default)]
    frozen: BTreeSet<HeapId>,
    /// Date's internal time value is not an enumerable JavaScript property.
    #[serde(default)]
    dates: BTreeMap<HeapId, JsNumber>,
    #[serde(default)]
    function_lengths: BTreeMap<String, usize>,
    #[serde(default)]
    prototypes: BTreeMap<HeapId, Option<HeapId>>,
    #[serde(default)]
    intrinsics: BTreeMap<String, HeapId>,
    #[serde(default)]
    non_enumerable: BTreeMap<HeapId, BTreeSet<String>>,
    #[serde(default)]
    builtin_getters: BTreeMap<HeapId, BTreeMap<String,String>>,
    #[serde(default)]
    boxed: BTreeMap<HeapId, VmValue>,
    #[serde(default)]
    constructable: BTreeSet<String>,
    #[serde(default)]
    realm_ready: bool,
    #[serde(default)]
    descriptors: BTreeMap<HeapId,BTreeMap<String,PropertyMeta>>,
    #[serde(default)]
    deleted_properties: BTreeMap<HeapId,BTreeSet<String>>,
    #[serde(default)]
    non_extensible: BTreeSet<HeapId>,
    #[serde(default)]
    next_symbol:u64,
    #[serde(default)]
    symbols:BTreeMap<u64,Option<VmValue>>,
    #[serde(default)]
    symbol_registry:Vec<(Vec<u16>,u64)>,
    #[serde(default)]
    well_known_symbols:BTreeMap<String,u64>,
}

// The existing interpreter will be migrated to these values incrementally.
#[allow(dead_code)]
impl Heap {
    /// The conformance helper is a test facility, not a new language type.
    #[cfg(test)]
    pub(crate) fn install_test_error(&mut self)->Result<()> {
        let ctor=self.native("test262.error")?;
        let proto=self.allocate(HeapNode::Object(vec![]))?;
        self.define_builtin(ctor,"prototype",VmValue::Reference(proto))?;
        self.define_builtin(proto,"constructor",VmValue::Reference(ctor))?;
        self.intrinsics.insert("Test262Error.prototype".into(),proto);
        Ok(())
    }
    pub(super) fn register_function_length(&mut self, id: &str, length: usize) {self.function_lengths.insert(id.into(),length);}
    pub(super) fn function_length(&self, id: &str) -> usize {
        self.function_lengths.get(id).copied().unwrap_or_else(||super::js_functions::native_length(id))
    }
    pub(super) fn native_reference(&self,name:&str)->Option<VmValue> {
        self.natives.get(name).copied().map(VmValue::Reference)
    }
    pub(super) fn ensure_json_cloneable(&self,value:&VmValue,seen:&mut BTreeSet<HeapId>)->Result<()> {
        let mut pending=if let VmValue::Reference(id)=value{vec![*id]}else{vec![]};
        while let Some(id)=pending.pop() {
            if !seen.insert(id){continue;}
            let mut visit=|value:&VmValue|if let VmValue::Reference(id)=value{pending.push(*id);};
            match self.get(id)? {
                HeapNode::Collection(_) | HeapNode::Iterator(_)=>bail!("TypeError: structuredClone currently supports JSON values only; collection and iterator cloning is not implemented. Runtime snapshots preserve them in full."),
                HeapNode::Array(values)=>for value in values {visit(value);},
                HeapNode::Object(properties)=>for (_,value) in properties {visit(value);},
                _=>{},
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn test_native(&self,name:&str)->Option<VmValue> {
        self.natives.get(name).map(|id|VmValue::Reference(*id))
    }
    /// Import a host JSON value into identity-bearing VM values. Each object
    /// or array becomes its own heap node; JSON itself cannot express aliases.
    pub(super) fn from_json(&mut self, input: &Value) -> Result<VmValue> {
        let first_new_id = self.next_id;
        let result = self.import_json(input, 0);
        if result.is_err() {
            self.nodes.retain(|id, _| id.0 < first_new_id);
            self.next_id = first_new_id;
        }
        result
    }

    fn import_json(&mut self, input: &Value, depth: usize) -> Result<VmValue> {
        if depth > 128 {
            bail!("JSON value exceeds maximum nesting depth");
        }
        Ok(match input {
            Value::Null => VmValue::Null,
            Value::Bool(value) => VmValue::Boolean(*value),
            Value::Number(value) => VmValue::Number(JsNumber::new(
                value.as_f64().context("invalid JSON number")?,
            )),
            Value::String(value) => VmValue::String(value.clone()),
            Value::Array(values) => {
                let values = values
                    .iter()
                    .map(|v| self.import_json(v, depth + 1))
                    .collect::<Result<Vec<_>>>()?;
                VmValue::Reference(self.allocate(HeapNode::Array(values))?)
            }
            Value::Object(values) => {
                let mut properties = Vec::new();
                for (key, value) in values {
                    properties.push((Self::string_key(key), self.import_json(value, depth + 1)?));
                }
                VmValue::Reference(self.allocate(HeapNode::Object(properties))?)
            }
        })
    }

    /// Tool arguments and results use JSON. Inside objects and arrays this
    /// follows JSON.stringify: undefined and functions are omitted from
    /// objects and become null in arrays. A cycle, a non-finite number or a
    /// top-level value without a JSON form is an error.
    pub(super) fn to_json(&self, value: &VmValue) -> Result<Value> {
        self.export_json(value, &mut BTreeSet::new(), 0)
    }

    /// Values that JSON.stringify leaves out of objects and writes as null in
    /// arrays: undefined and functions.
    pub(super) fn json_absent(&self, value: &VmValue) -> bool {
        match value {
            VmValue::Undefined | VmValue::Symbol(_) => true,
            VmValue::Reference(id) => matches!(
                self.nodes.get(id),
                Some(HeapNode::Closure { .. } | HeapNode::BoundFunction { .. } | HeapNode::Native(_) | HeapNode::PromiseResolver { .. })
            ),
            _ => false,
        }
    }

    /// The function value of a built-in or tool, the same node for every use.
    pub(super) fn native(&mut self, name: &str) -> Result<HeapId> {
        if let Some(id) = self.natives.get(name) {
            return Ok(*id);
        }
        let id = self.allocate(HeapNode::Native(name.to_owned()))?;
        self.natives.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Add an initialized binding to one environment, such as a hoisted function.
    pub(super) fn define_binding(&mut self, environment: HeapId, name: &str, value: VmValue) -> Result<()> {
        let (bindings, uninitialized) = self.environment_mut(environment)?;
        uninitialized.remove(name);
        bindings.insert(name.to_owned(), value);
        Ok(())
    }

    fn export_json(
        &self,
        value: &VmValue,
        active: &mut BTreeSet<HeapId>,
        depth: usize,
    ) -> Result<Value> {
        if depth > 128 {
            bail!("value exceeds maximum JSON nesting depth");
        }
        Ok(match value {
            VmValue::Symbol(_) => bail!("symbol cannot cross the JSON host boundary; use String or a data representation"),
            VmValue::Undefined => bail!("undefined cannot cross the JSON host boundary"),
            VmValue::Null => Value::Null,
            VmValue::Boolean(value) => Value::Bool(*value),
            VmValue::Number(value) => {
                let number = value.get();
                if !number.is_finite() {
                    bail!("non-finite number cannot cross the JSON host boundary");
                }
                super::number_value(number)
            }
            VmValue::String(value) => Value::String(value.clone()),
            VmValue::Utf16(_) => bail!("unpaired UTF-16 surrogate cannot cross the Unicode JSON host boundary; use JSON.stringify to preserve escaped code units"),
            VmValue::Promise(_) => bail!("promise cannot cross the JSON host boundary"),
            VmValue::Reference(id) => {
                if let Some(value)=self.boxed_value(*id).filter(|v|!matches!(v,VmValue::Symbol(_))){return self.export_json(value,active,depth+1);}
                if let Some(ms) = self.date_value(*id) {
                    return Ok(if ms.is_finite() { Value::String(super::js_date::iso(ms)?) } else { Value::Null });
                }
                for key in self.own_names(*id)?.into_iter().filter(|k|!Self::is_symbol_key(k)&&self.enumerable(*id,k)) {
                    ensure!(self.own_descriptor(*id,&key)?.is_none_or(|d|d.accessor.is_none()),"TypeError: accessor cannot cross the JSON host boundary; read its value or use JSON.stringify first");
                }
                if let HeapNode::Array(items)=self.get(*id)?{for i in 0..items.len(){ensure!(self.descriptor_owner(*id,&i.to_string())?.is_none_or(|(_,d)|d.accessor.is_none()),"TypeError: accessor cannot cross the JSON host boundary; materialize with Array.from or JSON.stringify first");}}
                if !active.insert(*id) {
                    bail!("cyclic value cannot cross the JSON host boundary");
                }
                let result = match self.get(*id)? {
                    HeapNode::Object(properties) => {
                        let mut output = serde_json::Map::new();
                        for (key, item) in properties {
                            if !Self::is_symbol_key(key) && self.enumerable(*id,key) && !self.json_absent(item) {
                                let VmValue::String(key)=self.key_value(key) else{unreachable!()};
                                output.insert(key, self.export_json(item, active, depth + 1)?);
                            }
                        }
                        Value::Object(output)
                    }
                    HeapNode::Collection(_) | HeapNode::Iterator(_)=>{
                        let mut output=serde_json::Map::new();
                        for (key,item) in self.array_properties(*id) {
                            if !Self::is_symbol_key(key) && self.enumerable(*id,key) && !self.json_absent(item){let VmValue::String(key)=self.key_value(key) else{unreachable!()};output.insert(key,self.export_json(item,active,depth+1)?);}
                        }
                        Value::Object(output)
                    }
                    HeapNode::Array(items) => Value::Array(
                        items
                            .iter()
                            .enumerate()
                            .map(|(index, item)| {
                                let inherited=if self.array_holes.get(id).is_some_and(|holes|holes.contains(&index)){self.lookup_property(*id,&index.to_string())?}else{None};
                                let item=inherited.as_ref().unwrap_or(item);
                                if self.json_absent(item)
                                    || self
                                        .array_holes
                                        .get(id)
                                        .is_some_and(|holes| holes.contains(&index)) && inherited.is_none()
                                {
                                    Ok(Value::Null)
                                } else {
                                    self.export_json(item, active, depth + 1)
                                }
                            })
                            .collect::<Result<Vec<_>>>()?,
                    ),
                    HeapNode::Promise { .. } => bail!("promise cannot cross the JSON host boundary"),
                    HeapNode::Environment { .. } | HeapNode::Closure { .. } | HeapNode::BoundFunction { .. } | HeapNode::Native(_) | HeapNode::PromiseCapability { .. } | HeapNode::PromiseResolver { .. } => {
                        bail!("function or environment cannot cross the JSON host boundary")
                    }
                };
                active.remove(id);
                result
            }
        })
    }

    pub(super) fn allocate(&mut self, node: HeapNode) -> Result<HeapId> {
        let id = HeapId(self.next_id);
        self.next_id = self.next_id.checked_add(1).context("heap id exhausted")?;
        self.nodes.insert(id, node);
        self.initialize_node(id)?;
        Ok(id)
    }

    pub(super) fn get(&self, id: HeapId) -> Result<&HeapNode> {
        self.nodes.get(&id).context("unknown heap reference")
    }
    pub(super) fn get_mut(&mut self, id: HeapId) -> Result<&mut HeapNode> {
        self.nodes.get_mut(&id).context("unknown heap reference")
    }

    /// Reclaim unreachable nodes between instructions, when the interpreter
    /// has no unsaved temporary Rust values on its stack.
    pub(super) fn collect(&mut self, roots: &[VmValue]) -> Result<usize> {
        let mut pending = Vec::new();
        for value in roots {
            if let VmValue::Reference(id) = value {
                pending.push(*id);
            }
        }
        // Built-in function values stay interned for the life of the program.
        pending.extend(self.natives.values().copied());
        pending.extend(self.intrinsics.values().copied());
        let mut live = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !live.insert(id) {
                continue;
            }
            pending.extend(self.prototypes.get(&id).copied().flatten());
            pending.extend(self.method_homes.get(&id).copied());
            if let Some(props)=self.descriptors.get(&id){for d in props.values(){if let Some((g,s))=&d.accessor{for v in [g,s]{if let VmValue::Reference(child)=v{pending.push(*child);}}}}}
            if let Some(VmValue::Reference(value))=self.boxed.get(&id){pending.push(*value);}
            if let Some(properties) = self.array_properties.get(&id) {
                for (_, value) in properties {
                    if let VmValue::Reference(child) = value { pending.push(*child); }
                }
            }
            let mut visit = |value: &VmValue| {
                if let VmValue::Reference(id) = value {
                    pending.push(*id);
                }
            };
            match self.get(id)? {
                HeapNode::Object(properties) => {
                    for (_, value) in properties {
                        visit(value);
                    }
                }
                HeapNode::Array(values) => {
                    for value in values {
                        visit(value);
                    }
                }
                HeapNode::Collection(c)=>for (key,value) in c.entries.iter().flatten(){visit(key);visit(value);},
                HeapNode::Iterator(it)=>if let Some(source)=&it.source {visit(source);},
                HeapNode::Environment {
                    parent, bindings, ..
                } => {
                    if let Some(parent) = parent {
                        pending.push(*parent);
                    }
                    for value in bindings.values() {
                        if let VmValue::Reference(id) = value {
                            pending.push(*id);
                        }
                    }
                }
                HeapNode::Closure { environment, .. } => pending.push(*environment),
                HeapNode::BoundFunction {target,receiver,args,..} => {visit(target);visit(receiver);for arg in args {visit(arg);}},
                HeapNode::Promise { outcome, .. } => {
                    if let Some(super::promise::Outcome::Fulfilled(value) | super::promise::Outcome::Rejected(value)) = outcome { visit(value); }
                }
                HeapNode::PromiseCapability { promise, .. } => pending.push(*promise),
                HeapNode::PromiseResolver { capability, .. } => pending.push(*capability),
                HeapNode::Native(_) => {}
            }
        }
        let before = self.nodes.len();
        self.nodes.retain(|id, _| live.contains(id));
        self.array_holes.retain(|id, _| live.contains(id));
        self.classes.retain(|id, _| live.contains(id));
        self.array_properties.retain(|id, _| live.contains(id));
        self.frozen.retain(|id| live.contains(id));
        self.dates.retain(|id, _| live.contains(id));
        self.prototypes.retain(|id,_|live.contains(id));
        self.class_constructors.retain(|id,_|live.contains(id));
        self.readonly_bindings.retain(|id,_|live.contains(id));
        self.method_homes.retain(|id,_|live.contains(id));
        self.non_enumerable.retain(|id,_|live.contains(id));
        self.builtin_getters.retain(|id,_|live.contains(id));
        self.boxed.retain(|id,_|live.contains(id));
        self.descriptors.retain(|id,_|live.contains(id));
        self.deleted_properties.retain(|id,_|live.contains(id));
        self.non_extensible.retain(|id|live.contains(id));
        Ok(before - self.nodes.len())
    }

    pub(super) fn append(&mut self, id: HeapId, items: Vec<VmValue>) -> Result<usize> {
        self.ensure_writable(id)?;
        match self.nodes.get_mut(&id).context("unknown heap reference")? {
            HeapNode::Array(values) => {
                values.extend(items);
                Ok(values.len())
            }
            _ => bail!("result collection requires an array"),
        }
    }

    pub(super) fn set_property(&mut self, id: HeapId, key: String, value: VmValue) -> Result<()> {
        let d=self.descriptor_owner(id,&key)?;
        if let Some((_,d))=d {ensure!(d.accessor.is_none()&&d.writable,"TypeError: Cannot assign to read-only property {key}");}
        let own=self.own_descriptor(id,&key)?.is_some();
        ensure!(own||self.extensible(id),"TypeError: Cannot add property {key}, object is not extensible");
        if matches!(self.get(id)?,HeapNode::Array(_))&&let Ok(index)=key.parse::<usize>() {
            let len=match self.get(id)?{HeapNode::Array(a)=>a.len(),_=>0};
            ensure!(index<len||self.own_descriptor(id,"length")?.unwrap().writable,"TypeError: Cannot extend a read-only array length");
        }
        if key=="length" && matches!(self.get(id)?,HeapNode::Array(_)) {
            let patch=self.allocate(HeapNode::Object(vec![("value".into(),value)]))?;
            ensure!(self.define_own(id,&key,patch)?,"TypeError: Cannot shrink array past a nonconfigurable property");return Ok(());
        }
        if let Some(keys)=self.deleted_properties.get_mut(&id){keys.remove(&key);}
        self.raw_set_property(id,key,value)
    }
    pub(super) fn raw_set_property(&mut self,id:HeapId,key:String,value:VmValue)->Result<()> {
        if matches!(self.get(id)?,HeapNode::Closure{..}|HeapNode::Native(_)|HeapNode::BoundFunction{..}|HeapNode::PromiseResolver{..}|HeapNode::Promise{..}) {
            let properties=self.array_properties.entry(id).or_default();
            if let Some((_,old))=properties.iter_mut().find(|(name,_)|*name==key){*old=value;}else{properties.push((key,value));}
            return Ok(());
        }
        match self.nodes.get_mut(&id).context("unknown heap reference")? {
            HeapNode::Object(properties) => {
                if let Some((_, existing)) = properties.iter_mut().find(|(name, _)| name == &key) {
                    *existing = value;
                } else {
                    properties.push((key, value));
                }
                Ok(())
            }
            HeapNode::Array(items) => {
                if key == "length" {
                    let VmValue::Number(length) = value else {
                        bail!("array length must be a number")
                    };
                    let length = length.get();
                    if !length.is_finite()
                        || length < 0.0
                        || length.fract() != 0.0
                        || length > 1_000_000.0
                    {
                        bail!("invalid array length");
                    }
                    let length = length as usize;
                    let holes = self.array_holes.entry(id).or_default();
                    if length > items.len() {
                        holes.extend(items.len()..length);
                        items.resize(length, VmValue::Undefined);
                    } else {
                        items.truncate(length);
                        holes.retain(|index| *index < length);
                    }
                    return Ok(());
                }
                let Some(index) = key.parse::<u32>().ok().filter(|n| *n != u32::MAX && n.to_string() == key).map(|n| n as usize) else {
                    let properties = self.array_properties.entry(id).or_default();
                    if let Some((_, existing)) = properties.iter_mut().find(|(name, _)| *name == key) { *existing = value; }
                    else { properties.push((key, value)); }
                    return Ok(());
                };
                if index > 1_000_000 {
                    bail!("array index exceeds runtime limit");
                }
                let holes = self.array_holes.entry(id).or_default();
                if index >= items.len() {
                    holes.extend(items.len()..index);
                    items.resize(index + 1, VmValue::Undefined);
                }
                holes.remove(&index);
                items[index] = value;
                Ok(())
            }
            HeapNode::Collection(_) | HeapNode::Iterator(_)=>{
                if !self.realm_ready && matches!(self.nodes.get(&id),Some(HeapNode::Collection(c)) if !c.kind.weak()) && key=="size" {
                    bail!("TypeError: Cannot set read-only collection size");
                }
                let properties=self.array_properties.entry(id).or_default();
                if let Some((_,old))=properties.iter_mut().find(|(name,_)|*name==key){*old=value;}else{properties.push((key,value));}
                Ok(())
            }
            _ => bail!("property assignment requires an object"),
        }
    }

    pub(super) fn class_of(&self, id: HeapId) -> Option<&str> {
        self.classes.get(&id).map(String::as_str)
    }

    pub(super) fn new_date(&mut self, ms: f64) -> Result<VmValue> {
        let id = self.allocate(HeapNode::Object(Vec::new()))?;
        self.set_class(id, "Date");
        self.dates.insert(id, JsNumber::new(ms));
        Ok(VmValue::Reference(id))
    }

    pub(super) fn date_value(&self, id: HeapId) -> Option<f64> {
        self.dates.get(&id).map(|n| n.get())
    }

    pub(super) fn freeze(&mut self, id: HeapId) { self.integrity(id,true).expect("valid heap integrity"); }

    pub(super) fn ensure_writable(&self, id: HeapId) -> Result<()> {
        if self.frozen.contains(&id) { bail!("TypeError: Cannot modify a frozen object"); }
        Ok(())
    }

    pub(super) fn array_properties(&self, id: HeapId) -> &[(String, VmValue)] {
        self.array_properties.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(super) fn set_class(&mut self, id: HeapId, class: &str) {
        if let Some(proto)=self.intrinsics.get(&format!("{class}.prototype")).copied(){self.prototypes.insert(id,Some(proto));}
        self.classes.insert(id, class.to_owned());
    }

    /// A regular expression value is the object `{"$flow_regex": {pattern, flags}}`.
    pub(super) fn regex_parts(&self, id: HeapId) -> Option<(String, String)> {
        let HeapNode::Object(properties) = self.get(id).ok()? else {
            return None;
        };
        let Some((_,VmValue::Reference(descriptor)))=properties.iter().find(|(key,_)|key=="$flow_regex") else {
            return None;
        };
        let pattern = self.property(*descriptor, "pattern").ok()?;
        let flags = self.property(*descriptor, "flags").ok()?;
        match (pattern, flags) {
            (VmValue::String(pattern), VmValue::String(flags)) => Some((pattern, flags)),
            _ => None,
        }
    }

    /// Mark an index as a hole after removing or never assigning it.
    pub(super) fn set_hole(&mut self, id: HeapId, index: usize) {
        self.array_holes.entry(id).or_default().insert(index);
    }

    /// Replace an array's elements and holes in one step.
    pub(super) fn replace_array(&mut self, id: HeapId, items: Vec<VmValue>, holes: BTreeSet<usize>) -> Result<()> {
        self.ensure_writable(id)?;
        match self.nodes.get_mut(&id).context("unknown heap reference")? {
            HeapNode::Array(values) => {
                *values = items;
                if holes.is_empty() {
                    self.array_holes.remove(&id);
                } else {
                    self.array_holes.insert(id, holes);
                }
                Ok(())
            }
            _ => bail!("receiver must be an array"),
        }
    }

    pub(super) fn holes(&self, id: HeapId) -> BTreeSet<usize> {
        self.array_holes.get(&id).cloned().unwrap_or_default()
    }

    /// The `delete` operator: an object loses the property, an array index
    /// becomes a hole. Other keys and values are left unchanged.
    pub(super) fn delete_property(&mut self, id: HeapId, key: &str) -> Result<()> {
        ensure!(self.own_descriptor(id,key)?.is_none_or(|d|d.configurable),"TypeError: Cannot delete nonconfigurable property {key}");
        self.raw_delete_property(id,key)?;
        self.descriptors.entry(id).or_default().remove(key);
        self.deleted_properties.entry(id).or_default().insert(key.into());
        if let Some(props)=self.builtin_getters.get_mut(&id){props.remove(key);}
        Ok(())
    }
    pub(super) fn raw_delete_property(&mut self,id:HeapId,key:&str)->Result<()> {
        if let Some(keys)=self.non_enumerable.get_mut(&id){keys.remove(key);}
        match self.nodes.get_mut(&id).context("unknown heap reference")? {
            HeapNode::Object(properties) => properties.retain(|(name, _)| name != key),
            HeapNode::Collection(_) | HeapNode::Iterator(_) | HeapNode::Closure {..} | HeapNode::Native(_) | HeapNode::BoundFunction {..} | HeapNode::PromiseResolver {..}=>if let Some(props)=self.array_properties.get_mut(&id){props.retain(|(name,_)|name!=key);},
            HeapNode::Array(items) => {
                if key == "length" { bail!("TypeError: Cannot delete array length"); }
                if let Some(properties) = self.array_properties.get_mut(&id) { properties.retain(|(name, _)| name != key); }
                if let Ok(index) = key.parse::<usize>()
                    && index.to_string() == key
                    && index < items.len()
                {
                    items[index] = VmValue::Undefined;
                    self.array_holes.entry(id).or_default().insert(index);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn has_array_index(&self, id: HeapId, index: usize) -> Result<bool> {
        let HeapNode::Array(items) = self.get(id)? else {
            bail!("receiver must be an array")
        };
        Ok(index < items.len()
            && !self
                .array_holes
                .get(&id)
                .is_some_and(|holes| holes.contains(&index)))
    }

    /// An object's own property, or None when it has no such property.
    pub(super) fn own_property(&self, id: HeapId, key: &str) -> Result<Option<VmValue>> {
        if self.deleted_properties.get(&id).is_some_and(|keys|keys.contains(key)){return Ok(None);}
        if self.descriptors.get(&id).and_then(|m|m.get(key)).is_some_and(|d|d.accessor.is_some()){return Ok(Some(VmValue::Undefined));}
        if key=="lastIndex" && self.regex_parts(id).is_some(){return Ok(Some(VmValue::Number(JsNumber::new(0.0))));}
        if self.builtin_getters.get(&id).is_some_and(|getters|getters.contains_key(key)){return Ok(Some(VmValue::Undefined));}
        if let Some(value)=self.boxed.get(&id).filter(|v|matches!(v,VmValue::String(_)|VmValue::Utf16(_))) {
            let text=super::js_value::string_units(self,value)?;
            if key=="length"{return Ok(Some(VmValue::Number(JsNumber::new(text.len() as f64))));}
            if let Ok(i)=key.parse::<usize>()&&i.to_string()==key&&let Some(c)=text.get(i){return Ok(Some(super::js_value::from_utf16(vec![*c])));}
        }
        if matches!(self.get(id)?,HeapNode::Closure{..}|HeapNode::Native(_)|HeapNode::BoundFunction{..}|HeapNode::PromiseResolver{..}) {
            if let Some((_,value))=self.array_properties(id).iter().find(|(name,_)|name==key){return Ok(Some(value.clone()));}
            return Ok(match (self.get(id)?,key) {
                (HeapNode::BoundFunction{name,..},"name")=>Some(VmValue::String(name.clone())),
                (HeapNode::BoundFunction{length,..},"length")=>Some(VmValue::Number(JsNumber::new(*length as f64))),
                (HeapNode::Closure{function,..}|HeapNode::Native(function),"name")=>Some(VmValue::String(super::js_value::function_name(function).into())),
                (HeapNode::Closure{function,..},"length")=>Some(VmValue::Number(JsNumber::new(self.function_length(function) as f64))),
                (HeapNode::Native(function),"length")=>Some(VmValue::Number(JsNumber::new(super::js_functions::native_length(function) as f64))),
                (HeapNode::PromiseResolver{..},"name")=>Some(VmValue::String(String::new())),
                (HeapNode::PromiseResolver{..},"length")=>Some(VmValue::Number(JsNumber::new(1.0))),
                _=>None,
            });
        }
        Ok(match self.get(id)? {
            HeapNode::Object(properties) => properties
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone()),
            HeapNode::Collection(_) | HeapNode::Iterator(_)=>self.array_properties(id).iter().find(|(name,_)|name==key).map(|(_,value)|value.clone()),
            HeapNode::Array(items) => {
                if key == "length" { Some(VmValue::Number(JsNumber::new(items.len() as f64))) }
                else if let Some(index) = key.parse::<usize>().ok().filter(|n| n.to_string() == key) {
                    if self.has_array_index(id, index)? { Some(items[index].clone()) } else { None }
                } else { self.array_properties(id).iter().find(|(name, _)| name == key).map(|(_, value)| value.clone()) }
            },
            _ => None,
        })
    }

    pub(super) fn property(&self, id: HeapId, key: &str) -> Result<VmValue> {
        match self.get(id)? {
            HeapNode::Object(properties) => Ok(properties
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
                .unwrap_or(VmValue::Undefined)),
            _ => bail!("property lookup requires an object"),
        }
    }

    /// A new environment whose `uninitialized` names stay in their temporal
    /// dead zone until declared.
    pub(super) fn new_environment(
        &mut self,
        parent: Option<HeapId>,
        bindings: BTreeMap<String, VmValue>,
        uninitialized: BTreeSet<String>,
    ) -> Result<HeapId> {
        let mut bindings = bindings;
        for name in &uninitialized {
            bindings.entry(name.clone()).or_insert(VmValue::Undefined);
        }
        self.allocate(HeapNode::Environment {
            parent,
            bindings,
            uninitialized,
        })
    }

    pub(super) fn environment_parent(&self, environment: HeapId) -> Result<Option<HeapId>> {
        match self.get(environment)? {
            HeapNode::Environment { parent, .. } => Ok(*parent),
            _ => bail!("reference is not a lexical environment"),
        }
    }

    /// A fresh copy with the same parent, for the per-iteration bindings of
    /// `for (let …)`: closures made in earlier iterations keep the old copy.
    pub(super) fn copy_environment(&mut self, environment: HeapId) -> Result<HeapId> {
        let node = match self.get(environment)? {
            node @ HeapNode::Environment { .. } => node.clone(),
            _ => bail!("reference is not a lexical environment"),
        };
        self.allocate(node)
    }

    /// Bindings of one environment that have been initialized, without its parents.
    pub(super) fn initialized_bindings(&self, environment: HeapId) -> Result<Vec<(String, VmValue)>> {
        match self.get(environment)? {
            HeapNode::Environment {
                bindings,
                uninitialized,
                ..
            } => Ok(bindings
                .iter()
                .filter(|(name, _)| !uninitialized.contains(*name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()),
            _ => bail!("reference is not a lexical environment"),
        }
    }

    /// The environment that binds `name`, innermost first.
    fn binding_owner(&self, mut environment: HeapId, name: &str) -> Result<HeapId> {
        // A cycle can only arise from an invalid or corrupted snapshot.
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(environment) {
                bail!("cyclic lexical environment");
            }
            match self.get(environment)? {
                HeapNode::Environment {
                    parent, bindings, ..
                } => {
                    if bindings.contains_key(name) {
                        return Ok(environment);
                    }
                    environment = match parent {
                        Some(parent) => *parent,
                        None => bail!("ReferenceError: {name} is not defined"),
                    };
                }
                _ => bail!("reference is not a lexical environment"),
            }
        }
    }

    fn environment_mut(
        &mut self,
        environment: HeapId,
    ) -> Result<(&mut BTreeMap<String, VmValue>, &mut BTreeSet<String>)> {
        match self
            .nodes
            .get_mut(&environment)
            .context("unknown heap reference")?
        {
            HeapNode::Environment {
                bindings,
                uninitialized,
                ..
            } => Ok((bindings, uninitialized)),
            _ => bail!("reference is not a lexical environment"),
        }
    }

    pub(super) fn lookup_binding(&self, environment: HeapId, name: &str) -> Result<VmValue> {
        let owner = self.binding_owner(environment, name)?;
        let HeapNode::Environment {
            bindings,
            uninitialized,
            ..
        } = self.get(owner)?
        else {
            bail!("reference is not a lexical environment")
        };
        if uninitialized.contains(name) {
            bail!("ReferenceError: Cannot access '{name}' before initialization");
        }
        Ok(bindings.get(name).cloned().unwrap_or(VmValue::Undefined))
    }

    /// Assignment to an existing binding; a let/const in its dead zone throws.
    pub(super) fn assign_binding(
        &mut self,
        environment: HeapId,
        name: &str,
        value: VmValue,
    ) -> Result<()> {
        let owner = self.binding_owner(environment, name)?;
        let (_, uninitialized) = self.environment_mut(owner)?;
        if uninitialized.contains(name) {
            bail!("ReferenceError: Cannot access '{name}' before initialization");
        }
        if self.readonly_bindings.get(&owner).is_some_and(|names|names.contains(name)){bail!("TypeError: Assignment to constant class binding {name}");}
        let (bindings,_)=self.environment_mut(owner)?;
        bindings.insert(name.to_owned(), value);
        Ok(())
    }

    /// The declaration itself: initialize the nearest binding and end its dead zone.
    pub(super) fn initialize_binding(
        &mut self,
        environment: HeapId,
        name: &str,
        value: VmValue,
    ) -> Result<()> {
        let owner = self.binding_owner(environment, name)?;
        let (bindings, uninitialized) = self.environment_mut(owner)?;
        uninitialized.remove(name);
        bindings.insert(name.to_owned(), value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn previous_object_snapshot_format_remains_readable() {
        let value = json!({"next_id":1,"nodes":{"0":{"Object":{"x":"Null"}}}});
        let heap: Heap = serde_json::from_value(value).unwrap();
        assert_eq!(heap.property(HeapId(0), "x").unwrap(), VmValue::Null);
    }

    #[test]
    fn collection_preserves_reachable_cycles_and_removes_unreachable_nodes() {
        let mut heap = Heap::default();
        let live = heap.allocate(HeapNode::Object(Vec::new())).unwrap();
        let dead = heap.allocate(HeapNode::Object(Vec::new())).unwrap();
        heap.set_property(live, "self".into(), VmValue::Reference(live))
            .unwrap();
        heap.set_property(dead, "self".into(), VmValue::Reference(dead))
            .unwrap();
        assert_eq!(heap.collect(&[VmValue::Reference(live)]).unwrap(), 1);
        assert!(heap.get(live).is_ok());
        assert!(heap.get(dead).is_err());
    }

    #[test]
    fn javascript_numbers_survive_snapshot_without_json_number_loss() {
        let heap = Heap::default();
        for number in [-0.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            let original = VmValue::Number(JsNumber::new(number));
            let encoded = serde_json::to_vec(&original).unwrap();
            let restored: VmValue = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(original, restored);
            if !number.is_finite() {
                assert!(heap.to_json(&restored).is_err());
            }
        }
    }

    #[test]
    fn failed_import_does_not_leave_partial_heap_nodes() {
        let mut heap = Heap::default();
        let existing = heap.from_json(&json!({"existing": true})).unwrap();
        let initial_count = heap.nodes.len();
        let mut deep = json!(0);
        for _ in 0..130 {
            deep = json!([deep]);
        }
        assert!(heap.from_json(&json!([{}, deep])).is_err());
        assert_eq!(heap.nodes.len(), initial_count);
        assert_eq!(heap.to_json(&existing).unwrap(), json!({"existing": true}));
    }

    #[test]
    fn json_boundary_preserves_shared_references_and_rejects_cycles() {
        let mut heap = Heap::default();
        let shared = heap
            .from_json(&json!({"name":"before", "items":[1,2]}))
            .unwrap();
        let id = match shared {
            VmValue::Reference(id) => id,
            _ => panic!("expected object"),
        };
        let alias = shared.clone();
        heap.set_property(id, "name".into(), VmValue::String("after".into()))
            .unwrap();
        let root = VmValue::Reference(heap.allocate(HeapNode::Array(vec![shared, alias])).unwrap());
        let restored: Heap = serde_json::from_slice(&serde_json::to_vec(&heap).unwrap()).unwrap();
        assert_eq!(
            restored.to_json(&root).unwrap(),
            json!([
                {"name":"after", "items":[1,2]}, {"name":"after", "items":[1,2]}
            ])
        );
        heap.set_property(id, "self".into(), VmValue::Reference(id))
            .unwrap();
        assert!(
            heap.to_json(&root)
                .unwrap_err()
                .to_string()
                .contains("cyclic")
        );
        assert!(heap.to_json(&VmValue::Undefined).is_err());
    }

    #[test]
    fn json_boundary_follows_json_stringify_for_undefined() {
        // As JSON.stringify: omitted from objects, null in arrays.
        let mut heap = Heap::default();
        let object = heap
            .allocate(HeapNode::Object(vec![
                ("kept".into(), VmValue::Null),
                ("dropped".into(), VmValue::Undefined),
            ]))
            .unwrap();
        let array = heap
            .allocate(HeapNode::Array(vec![
                VmValue::Undefined,
                VmValue::Reference(object),
            ]))
            .unwrap();
        assert_eq!(
            heap.to_json(&VmValue::Reference(array)).unwrap(),
            json!([null, {"kept": null}])
        );
    }

    #[test]
    fn shared_object_identity_and_cycles_survive_restore() {
        let mut heap = Heap::default();
        let object = heap.allocate(HeapNode::Object(Vec::new())).unwrap();
        let alias = object;
        heap.set_property(object, "self".into(), VmValue::Reference(object))
            .unwrap();
        heap.set_property(alias, "value".into(), VmValue::String("updated".into()))
            .unwrap();
        let bytes = serde_json::to_vec(&heap).unwrap();
        let restored: Heap = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            restored.property(object, "self").unwrap(),
            VmValue::Reference(object)
        );
        assert_eq!(
            restored.property(object, "value").unwrap(),
            VmValue::String("updated".into())
        );
        assert_eq!(
            restored.property(object, "missing").unwrap(),
            VmValue::Undefined
        );
    }

    #[test]
    fn closures_share_captured_bindings_after_restore() {
        let mut heap = Heap::default();
        let outer = heap
            .new_environment(
                None,
                [("count".into(), VmValue::Number(JsNumber::new(0.0)))].into(),
                BTreeSet::new(),
            )
            .unwrap();
        let inner = heap
            .new_environment(Some(outer), BTreeMap::new(), BTreeSet::new())
            .unwrap();
        let closure = heap
            .allocate(HeapNode::Closure {
                function: "increment".into(),
                environment: inner,
            })
            .unwrap();
        let bytes = serde_json::to_vec(&heap).unwrap();
        let mut restored: Heap = serde_json::from_slice(&bytes).unwrap();
        restored
            .assign_binding(inner, "count", VmValue::Number(JsNumber::new(1.0)))
            .unwrap();
        assert_eq!(
            restored.lookup_binding(outer, "count").unwrap(),
            VmValue::Number(JsNumber::new(1.0))
        );
        assert_eq!(
            restored.get(closure).unwrap(),
            &HeapNode::Closure {
                function: "increment".into(),
                environment: inner
            }
        );
    }

    #[test]
    fn dead_zone_shadowing_and_per_iteration_copies() {
        let mut heap = Heap::default();
        let outer = heap
            .new_environment(None, [("x".into(), VmValue::String("outer".into()))].into(), BTreeSet::new())
            .unwrap();
        // An inner let shadows the outer binding even before it is declared.
        let inner = heap
            .new_environment(Some(outer), BTreeMap::new(), ["x".to_owned()].into())
            .unwrap();
        let error = heap.lookup_binding(inner, "x").unwrap_err().to_string();
        assert!(error.contains("before initialization"), "{error}");
        assert!(heap.assign_binding(inner, "x", VmValue::Null).is_err());
        heap.initialize_binding(inner, "x", VmValue::String("inner".into())).unwrap();
        assert_eq!(heap.lookup_binding(inner, "x").unwrap(), VmValue::String("inner".into()));
        assert_eq!(heap.lookup_binding(outer, "x").unwrap(), VmValue::String("outer".into()));
        let copy = heap.copy_environment(inner).unwrap();
        heap.assign_binding(copy, "x", VmValue::String("next".into())).unwrap();
        assert_eq!(heap.lookup_binding(inner, "x").unwrap(), VmValue::String("inner".into()));
        assert_eq!(heap.environment_parent(copy).unwrap(), Some(outer));
        let missing = heap.lookup_binding(inner, "nothing").unwrap_err().to_string();
        assert!(missing.starts_with("ReferenceError: nothing is not defined"), "{missing}");
    }
}
