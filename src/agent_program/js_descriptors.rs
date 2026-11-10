//! Durable ordinary property attributes. User code is invoked by Runtime,
//! never by the heap's synchronous lookup/validation operations.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PropertyMeta {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
    pub accessor: Option<(VmValue, VmValue)>,
}

impl Heap {
    /// Ordinary data writes need no JS frame or descriptor-plan allocation.
    /// Anything capable of calling user code stays on the resumable path.
    pub(crate) fn try_fast_set(&mut self, target: &VmValue, key: &VmValue, value: &VmValue) -> Result<bool> {
        let VmValue::Reference(id) = target else { return Ok(false) };
        if matches!(key, VmValue::Reference(_)) || !matches!(self.get(*id)?, HeapNode::Object(_) | HeapNode::Array(_))
            || self.classes.contains_key(id) || self.boxed.contains_key(id) || self.frozen.contains(id) {
            return Ok(false);
        }
        let key = self.property_key(key)?;
        if Self::is_private_key(&key) { return Ok(false); }
        let array = matches!(self.get(*id)?, HeapNode::Array(_));
        if array && key == "length" { return Ok(false); }
        let descriptor = self.descriptor_owner(*id, &key)?;
        if descriptor.is_some_and(|(_, d)| d.accessor.is_some() || !d.writable) { return Ok(false); }
        if self.own_descriptor(*id, &key)?.is_none() && !self.extensible(*id) { return Ok(false); }
        if array && let Some(index) = array_index(&key) {
            let HeapNode::Array(items) = self.get(*id)? else { unreachable!() };
            if index >= 1_000_000 || index >= items.len() && !self.own_descriptor(*id, "length")?.unwrap().writable {
                return Ok(false);
            }
        }
        self.set_property(*id, key, value.clone())?;
        Ok(true)
    }

    /// Check every append target before touching the array. If a setter or
    /// read-only inherited index exists, the slow path must preserve partial
    /// writes and observable setter ordering.
    pub(crate) fn try_fast_push(&mut self, receiver: &VmValue, args: &[VmValue]) -> Result<Option<usize>> {
        let VmValue::Reference(id) = receiver else { return Ok(None) };
        let HeapNode::Array(items) = self.get(*id)? else { return Ok(None) };
        let len = items.len();
        if !self.extensible(*id) || !self.own_descriptor(*id, "length")?.unwrap().writable {
            return Ok(None);
        }
        if len.saturating_add(args.len()) > 1_000_000 { return Ok(None); }
        if let Some(proto) = self.prototype_of(*id) {
            for index in len..len + args.len() {
                if self.descriptor_owner(proto, &index.to_string())?
                    .is_some_and(|(_, d)| d.accessor.is_some() || !d.writable) { return Ok(None); }
            }
        }
        self.append(*id, args.to_vec()).map(Some)
    }

    pub(crate) fn own_descriptor(&self, id: HeapId, key: &str) -> Result<Option<PropertyMeta>> {
        if self
            .deleted_properties
            .get(&id)
            .is_some_and(|keys| keys.contains(key))
        {
            return Ok(None);
        }
        if let Some(meta) = self.descriptors.get(&id).and_then(|m| m.get(key)) {
            return Ok(Some(meta.clone()));
        }
        if self.own_property(id, key)?.is_none() {
            return Ok(None);
        }
        let function = matches!(
            self.get(id)?,
            HeapNode::Closure { .. }
                | HeapNode::BoundFunction { .. }
                | HeapNode::Native(_)
                | HeapNode::PromiseResolver { .. }
        );
        let length = key == "length" && matches!(self.get(id)?, HeapNode::Array(_));
        let intrinsic_prototype = key == "prototype"
            && matches!(self.get(id)?,HeapNode::Native(name) if super::super::js_prototype_global(name));
        let ordinary_prototype =
            key == "prototype" && matches!(self.get(id)?, HeapNode::Closure { .. });
        let string_index = self
            .boxed
            .get(&id)
            .is_some_and(|v| matches!(v, VmValue::String(_) | VmValue::Utf16(_)))
            && (key == "length" || key.parse::<usize>().is_ok());
        let constant = self.readonly_constant(id, key);
        let readonly = constant
            || intrinsic_prototype
            || string_index
            || (function && matches!(key, "name" | "length"));
        Ok(Some(PropertyMeta {
            writable: !readonly,
            enumerable: self.enumerable(id, key),
            configurable: !(constant
                || intrinsic_prototype
                || ordinary_prototype
                || length
                || string_index
                || key == "lastIndex" && self.regex_parts(id).is_some()),
            accessor: None,
        }))
    }
    pub(crate) fn descriptor_owner(
        &self,
        id: HeapId,
        key: &str,
    ) -> Result<Option<(HeapId, PropertyMeta)>> {
        let mut at = Some(id);
        while let Some(owner) = at {
            if let Some(d) = self.own_descriptor(owner, key)? {
                return Ok(Some((owner, d)));
            }
            at = self.prototype_of(owner);
        }
        Ok(None)
    }
    pub(crate) fn extensible(&self, id: HeapId) -> bool {
        !self.non_extensible.contains(&id) && !self.frozen.contains(&id)
    }
    pub(crate) fn prevent_extensions(&mut self, id: HeapId) {
        self.non_extensible.insert(id);
    }
    pub(crate) fn descriptor_object(&mut self, id: HeapId, key: &str) -> Result<VmValue> {
        let Some(d) = self.own_descriptor(id, key)? else {
            return Ok(VmValue::Undefined);
        };
        let mut props = if let Some((get, set)) = d.accessor {
            vec![("get".into(), get), ("set".into(), set)]
        } else {
            vec![
                (
                    "value".into(),
                    self.own_property(id, key)?.unwrap_or(VmValue::Undefined),
                ),
                ("writable".into(), VmValue::Boolean(d.writable)),
            ]
        };
        props.push(("enumerable".into(), VmValue::Boolean(d.enumerable)));
        props.push(("configurable".into(), VmValue::Boolean(d.configurable)));
        Ok(VmValue::Reference(self.allocate(HeapNode::Object(props))?))
    }
    pub(crate) fn define_own(&mut self, id: HeapId, key: &str, patch: HeapId) -> Result<bool> {
        let field = |name: &str| self.own_property(patch, name);
        let value = field("value")?;
        let writable = field("writable")?;
        let enumerable = field("enumerable")?;
        let configurable = field("configurable")?;
        let get = field("get")?;
        let set = field("set")?;
        let access = get.is_some() || set.is_some();
        let data = value.is_some() || writable.is_some();
        ensure!(
            !(access && data),
            "TypeError: Invalid property descriptor: cannot mix data and accessors"
        );
        for f in [&get, &set].into_iter().flatten() {
            ensure!(
                *f == VmValue::Undefined || super::super::vm_eval::callable(self, f),
                "TypeError: Property accessor must be callable or undefined"
            );
        }
        let current = self.own_descriptor(id, key)?;
        if current.is_none() && !self.extensible(id) {
            return Ok(false);
        }
        if let Some(c) = &current {
            if !c.configurable {
                if configurable
                    .as_ref()
                    .is_some_and(super::super::vm_eval::truthy)
                    || enumerable
                        .as_ref()
                        .is_some_and(|v| super::super::vm_eval::truthy(v) != c.enumerable)
                {
                    return Ok(false);
                }
                if (access && c.accessor.is_none()) || (data && c.accessor.is_some()) {
                    return Ok(false);
                }
                if let Some((cg, cs)) = &c.accessor {
                    if get.as_ref().is_some_and(|g| !same_value(g, cg))
                        || set.as_ref().is_some_and(|s| !same_value(s, cs))
                    {
                        return Ok(false);
                    }
                } else if !c.writable {
                    if writable.as_ref().is_some_and(super::super::vm_eval::truthy)
                        || value.as_ref().is_some_and(|v| {
                            !same_value(
                                v,
                                &self
                                    .own_property(id, key)
                                    .ok()
                                    .flatten()
                                    .unwrap_or(VmValue::Undefined),
                            )
                        })
                    {
                        return Ok(false);
                    }
                }
            }
        }
        let mut next = current.clone().unwrap_or(PropertyMeta {
            writable: false,
            enumerable: false,
            configurable: false,
            accessor: None,
        });
        if access && next.accessor.is_none() {
            next.accessor = Some((VmValue::Undefined, VmValue::Undefined));
            next.writable = false;
        }
        if data && next.accessor.is_some() {
            next.accessor = None;
            next.writable = false;
        }
        if let Some(v) = writable {
            next.writable = super::super::vm_eval::truthy(&v);
        }
        if let Some(v) = enumerable {
            next.enumerable = super::super::vm_eval::truthy(&v);
        }
        if let Some(v) = configurable {
            next.configurable = super::super::vm_eval::truthy(&v);
        }
        if let Some((g, s)) = &mut next.accessor {
            if let Some(v) = get {
                *g = v;
            }
            if let Some(v) = set {
                *s = v;
            }
        }
        // ArraySetLength deletes descending indices; a nonconfigurable index
        // leaves the partial shrink visible, including a requested readonly length.
        if key == "length" && matches!(self.get(id)?, HeapNode::Array(_)) {
            if let Some(v) = &value {
                let n = super::super::js_value::to_number(self, v)?;
                ensure!(
                    n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= 1_000_000.0,
                    "RangeError: Invalid array length"
                );
                let old = match self.get(id)? {
                    HeapNode::Array(a) => a.len(),
                    _ => 0,
                };
                if n < (old as f64) {
                    for i in (n as usize..old).rev() {
                        if self
                            .own_descriptor(id, &i.to_string())?
                            .is_some_and(|d| !d.configurable)
                        {
                            self.raw_set_property(
                                id,
                                "length".into(),
                                VmValue::Number(JsNumber::new((i + 1) as f64)),
                            )?;
                            self.descriptors
                                .entry(id)
                                .or_default()
                                .insert(key.into(), next);
                            return Ok(false);
                        }
                        self.raw_delete_property(id, &i.to_string())?;
                        self.descriptors
                            .entry(id)
                            .or_default()
                            .remove(&i.to_string());
                    }
                }
            }
        }
        if matches!(self.get(id)?, HeapNode::Array(_))
            && let Some(i) = array_index(key)
        {
            let len = match self.get(id)? {
                HeapNode::Array(a) => a.len(),
                _ => 0,
            };
            if i >= len && !self.own_descriptor(id, "length")?.unwrap().writable {
                return Ok(false);
            }
        }
        let stored = if next.accessor.is_some() {
            VmValue::Undefined
        } else {
            value.unwrap_or_else(|| {
                if current.as_ref().is_some_and(|d| d.accessor.is_none()) {
                    self.own_property(id, key)
                        .ok()
                        .flatten()
                        .unwrap_or(VmValue::Undefined)
                } else {
                    VmValue::Undefined
                }
            })
        };
        self.raw_set_property(id, key.into(), stored)?;
        if let Some(keys) = self.deleted_properties.get_mut(&id) {
            keys.remove(key);
        }
        if let Some(keys) = self.builtin_getters.get_mut(&id) {
            keys.remove(key);
        }
        self.descriptors
            .entry(id)
            .or_default()
            .insert(key.into(), next);
        Ok(true)
    }
    pub(crate) fn integrity(&mut self, id: HeapId, frozen: bool) -> Result<()> {
        for key in self.own_names(id)? {
            if let Some(mut d) = self.own_descriptor(id, &key)? {
                d.configurable = false;
                if frozen && d.accessor.is_none() {
                    d.writable = false;
                }
                self.descriptors.entry(id).or_default().insert(key, d);
            }
        }
        self.prevent_extensions(id);
        if frozen {
            self.frozen.insert(id);
        }
        Ok(())
    }
    pub(crate) fn test_integrity(&self, id: HeapId, frozen: bool) -> Result<bool> {
        if self.extensible(id) {
            return Ok(false);
        }
        for key in self.own_names(id)? {
            if let Some(d) = self.own_descriptor(id, &key)? {
                if d.configurable || (frozen && d.accessor.is_none() && d.writable) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
    pub(crate) fn descriptor_builtin(
        &mut self,
        name: &str,
        args: &[VmValue],
    ) -> Result<Option<VmValue>> {
        let first = args.first().cloned().unwrap_or(VmValue::Undefined);
        let reflect = name.starts_with("Reflect.");
        let strict_object = |v: &VmValue| -> Result<HeapId> {
            if let VmValue::Reference(id) = v {
                Ok(*id)
            } else {
                bail!("TypeError: Reflect target must be an object")
            }
        };
        let target = match &first {
            VmValue::Reference(id) => Some(*id),
            _ => None,
        };
        let value = match name {
            "$$keyOperation" => {
                let mode = super::super::js_value::to_string(self, &first)?;
                return self.descriptor_builtin(&mode, &args[1..]);
            }
            "$$defineOwn" => {
                let id = strict_object(&first)?;
                let key = self.property_key(&args[1])?;
                let p = strict_object(&args[2])?;
                VmValue::Boolean(self.define_own(id, &key, p)?)
            }
            "Object.getOwnPropertyDescriptor" | "Reflect.getOwnPropertyDescriptor" => {
                let id = if reflect {
                    strict_object(&first)?
                } else {
                    let v = self.box_value(first.clone())?;
                    strict_object(&v)?
                };
                let key = self.property_key(args.get(1).unwrap_or(&VmValue::Undefined))?;
                self.descriptor_object(id, &key)?
            }
            "Object.getOwnPropertyNames" | "Object.getOwnPropertySymbols" | "Reflect.ownKeys" => {
                let id = if reflect {
                    strict_object(&first)?
                } else {
                    let v = self.box_value(first.clone())?;
                    strict_object(&v)?
                };
                let names=self.own_names(id)?.into_iter().filter(|key|match name{"Object.getOwnPropertyNames"=>!Self::is_symbol_key(key),"Object.getOwnPropertySymbols"=>Self::is_symbol_key(key),_=>true}).map(|key|self.key_value(&key)).collect();
                VmValue::Reference(self.allocate(HeapNode::Array(names))?)
            }
            "Object.preventExtensions"
            | "Object.seal"
            | "Object.freeze"
            | "Reflect.preventExtensions" => {
                if reflect {
                    strict_object(&first)?;
                }
                if let Some(id) = target {
                    if name == "Object.seal" || name == "Object.freeze" {
                        self.integrity(id, name == "Object.freeze")?;
                    } else {
                        self.prevent_extensions(id);
                    }
                }
                if reflect {
                    VmValue::Boolean(true)
                } else {
                    first
                }
            }
            "Object.isExtensible"
            | "Object.isSealed"
            | "Object.isFrozen"
            | "Reflect.isExtensible" => {
                if reflect {
                    strict_object(&first)?;
                }
                VmValue::Boolean(match target {
                    Some(id) => {
                        if name.ends_with("isExtensible") {
                            self.extensible(id)
                        } else {
                            self.test_integrity(id, name.ends_with("isFrozen"))?
                        }
                    }
                    None => !name.ends_with("isExtensible"),
                })
            }
            "Reflect.has" => {
                let id = strict_object(&first)?;
                let key = self.property_key(args.get(1).unwrap_or(&VmValue::Undefined))?;
                VmValue::Boolean(self.has_property(id, &key)?)
            }
            "Reflect.deleteProperty" => {
                let id = strict_object(&first)?;
                let key = self.property_key(args.get(1).unwrap_or(&VmValue::Undefined))?;
                let can = self
                    .own_descriptor(id, &key)?
                    .is_none_or(|d| d.configurable);
                if can {
                    self.delete_property(id, &key)?;
                }
                VmValue::Boolean(can)
            }
            "Reflect.getPrototypeOf" => self
                .prototype_of(strict_object(&first)?)
                .map(VmValue::Reference)
                .unwrap_or(VmValue::Null),
            "Reflect.setPrototypeOf" => {
                let id = strict_object(&first)?;
                let proto = match args.get(1) {
                    Some(VmValue::Null) => None,
                    Some(VmValue::Reference(p)) => Some(*p),
                    _ => bail!("TypeError: Prototype must be an object or null"),
                };
                VmValue::Boolean(self.set_prototype(id, proto).is_ok())
            }
            "$$arrayLikeOnly" => VmValue::Boolean(match &first {
                VmValue::Reference(id) => !matches!(
                    self.get(*id)?,
                    HeapNode::Array(_) | HeapNode::Collection(_) | HeapNode::Iterator(_)
                ),
                VmValue::String(_) | VmValue::Utf16(_) => false,
                _ => true,
            }),
            "$$iteratorPlan" => {
                let id = strict_object(&first)?;
                let HeapNode::Iterator(state) = self.get(id)? else {
                    bail!("TypeError: iterator receiver required")
                };
                let state = state.clone();
                let indirect=state.limit.is_some()||state.source.as_ref().is_some_and(|v|matches!(v,VmValue::Reference(source) if matches!(self.get(*source),Ok(HeapNode::Array(_)))));
                let mut props = vec![];
                if indirect {
                    let len = if let Some(limit) = state.limit {
                        limit
                    } else if let Some(VmValue::Reference(source)) = &state.source {
                        match self.get(*source)? {
                            HeapNode::Array(a) => a.len(),
                            _ => 0,
                        }
                    } else {
                        0
                    };
                    let done = state.source.is_none() || state.cursor >= len;
                    props.push(("done".into(), VmValue::Boolean(done)));
                    if let HeapNode::Iterator(it) = self.get_mut(id)? {
                        if done {
                            it.source = None;
                        } else {
                            it.cursor += 1;
                        }
                    }
                    if !done {
                        props.extend([
                            ("indirect".into(), VmValue::Boolean(true)),
                            ("source".into(), state.source.unwrap()),
                            (
                                "index".into(),
                                VmValue::Number(JsNumber::new(state.cursor as f64)),
                            ),
                            (
                                "kind".into(),
                                VmValue::String(
                                    match state.kind {
                                        super::super::js_collections::Iteration::Keys => "keys",
                                        super::super::js_collections::Iteration::Entries => {
                                            "entries"
                                        }
                                        _ => "values",
                                    }
                                    .into(),
                                ),
                            ),
                        ]);
                    }
                } else {
                    let value = super::super::js_collections::next(self, id)?;
                    props.extend([
                        ("done".into(), VmValue::Boolean(value.is_none())),
                        ("value".into(), value.unwrap_or(VmValue::Undefined)),
                        ("indirect".into(), VmValue::Boolean(false)),
                    ]);
                }
                VmValue::Reference(self.allocate(HeapNode::Object(props))?)
            }
            "$$boundTarget" => {
                if let Some(id) = target {
                    if let HeapNode::BoundFunction { target, .. } = self.get(id)? {
                        target.clone()
                    } else {
                        VmValue::Undefined
                    }
                } else {
                    VmValue::Undefined
                }
            }
            "$$hasPrototype" => {
                let id = strict_object(&first)?;
                let proto = strict_object(&args[1])?;
                VmValue::Boolean(self.has_prototype(id, proto)?)
            }
            "$$requireCoercible" => {
                ensure!(
                    !matches!(first, VmValue::Null | VmValue::Undefined),
                    "TypeError: Cannot convert null or undefined to object"
                );
                first
            }
            "$$jsonUnbox" => {
                if let Some(id) = target {
                    self.boxed.get(&id).filter(|v|!matches!(v,VmValue::Symbol(_))).cloned().unwrap_or(first)
                } else {
                    first
                }
            }
            "$$createObject" => {
                let proto = match first {
                    VmValue::Reference(p) => Some(p),
                    VmValue::Null => None,
                    _ => bail!("TypeError: Object.create prototype must be object or null"),
                };
                let id = self.allocate(HeapNode::Object(vec![]))?;
                self.set_prototype(id, proto)?;
                VmValue::Reference(id)
            }
            "$$createData" => {
                let id = strict_object(&first)?;
                let key = self.property_key(&args[1])?;
                let patch = self.allocate(HeapNode::Object(vec![
                    ("value".into(), args[2].clone()),
                    ("writable".into(), VmValue::Boolean(true)),
                    ("enumerable".into(), VmValue::Boolean(true)),
                    ("configurable".into(), VmValue::Boolean(true)),
                ]))?;
                ensure!(
                    self.define_own(id, &key, patch)?,
                    "TypeError: Cannot create property {key}"
                );
                first
            }
            "$$literalAccessor" => {
                let id = strict_object(&first)?;
                let key = self.property_key(&args[1])?;
                let kind = super::super::js_value::to_string(self, &args[3])?;
                if let VmValue::Reference(f) = args[2] {
                    self.raw_set_property(
                        f,
                        "name".into(),
                        self.function_name_key(&args[1],&kind)?,
                    )?;
                }
                let patch = self.allocate(HeapNode::Object(vec![
                    (kind, args[2].clone()),
                    ("enumerable".into(), VmValue::Boolean(true)),
                    ("configurable".into(), VmValue::Boolean(true)),
                ]))?;
                ensure!(
                    self.define_own(id, &key, patch)?,
                    "TypeError: Cannot create accessor {key}"
                );
                first
            }
            "$$prepareSet" => {
                let key = self.property_key(&args[1])?;
                if Self::is_private_key(&key) {
                    let VmValue::Reference(id) = first else { bail!("TypeError: private element requires an object") };
                    let setter = self.private_set(id,&key,args[2].clone())?;
                    return Ok(Some(VmValue::Reference(self.allocate(HeapNode::Array(vec![VmValue::Boolean(true),setter]))?)));
                }
                let regex_id = target.or_else(|| match &first { VmValue::Reference(id) => Some(*id), _ => None });
                if key == "lastIndex" && regex_id.is_some_and(|id| self.regex_parts(id).is_some()) {
                    let id = regex_id.unwrap();
                    self.set_property(id, key, args[2].clone())?;
                    return Ok(Some(VmValue::Reference(self.allocate(HeapNode::Array(vec![VmValue::Boolean(true), VmValue::Undefined]))?)));
                }
                let receiver = &args[3];
                let owner = target.or_else(|| self.primitive_prototype(&first));
                let descriptor = match owner {
                    Some(id) => self.descriptor_owner(id, &key)?,
                    None => None,
                };
                let (allowed, setter) = if let Some((_, d)) =
                    descriptor.as_ref().filter(|(_, d)| d.accessor.is_some())
                {
                    let setter = d.accessor.as_ref().unwrap().1.clone();
                    (setter != VmValue::Undefined, setter)
                } else if descriptor.is_some_and(|(_, d)| !d.writable) {
                    (false, VmValue::Undefined)
                } else if let VmValue::Reference(id) = receiver {
                    let own = self.own_descriptor(*id, &key)?;
                    if own
                        .as_ref()
                        .is_some_and(|d| d.accessor.is_some() || !d.writable)
                        || own.is_none() && !self.extensible(*id)
                    {
                        (false, VmValue::Undefined)
                    } else {
                        let patch = if own.is_some() {
                            vec![("value".into(), args[2].clone())]
                        } else {
                            vec![
                                ("value".into(), args[2].clone()),
                                ("writable".into(), VmValue::Boolean(true)),
                                ("enumerable".into(), VmValue::Boolean(true)),
                                ("configurable".into(), VmValue::Boolean(true)),
                            ]
                        };
                        let patch = self.allocate(HeapNode::Object(patch))?;
                        (self.define_own(*id, &key, patch)?, VmValue::Undefined)
                    }
                } else {
                    (false, VmValue::Undefined)
                };
                VmValue::Reference(
                    self.allocate(HeapNode::Array(vec![VmValue::Boolean(allowed), setter]))?,
                )
            }
            "$$builtinStringIterator"=>super::super::js_collections::iterator(self,first,None,false)?,
            "$$rawDateConstruct"=>{let args=super::super::js_functions::apply_args(self,first)?;super::super::js_date::construct(self,&args)?},
            "$$rawNumber"=>return super::super::js_methods::builtin(self,"Number",args).map(Some),
            "$$rawString"=>return super::super::js_methods::builtin(self,"String",args).map(Some),
            "$$rawSymbol"=>return self.symbol_builtin("Symbol",args),
            "$$rawSymbolFor"=>return self.symbol_builtin("Symbol.for",args),
            "$$rawLooseEqual"=>VmValue::Boolean(super::super::js_value::loose_equal(self,&first,&args[1])?),
            "$$isDate"=>VmValue::Boolean(target.is_some_and(|id|self.date_value(id).is_some())),
            "$$emptyCollection"=>{let name=super::super::js_value::to_string(self,&first)?;super::super::js_collections::construct(self,&name,&[])?},
            "$$defaultTag"=>VmValue::String(self.object_tag(&first)?),
            "$$isPromise" => VmValue::Boolean(target.is_some_and(|id|matches!(self.get(id),Ok(HeapNode::Promise{..})))),
            "$$isObject" => VmValue::Boolean(target.is_some()),
            "$$isConstructor" => VmValue::Boolean(self.is_constructor_value(&first)?),
            "$$rawStringify" => {
                return super::super::js_methods::builtin(self, "JSON.stringify", args).map(Some);
            }
            "$$keys" => {
                return super::super::js_methods::builtin(self, "Object.keys", args).map(Some);
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
    pub(crate) fn is_constructor_value(&self, v: &VmValue) -> Result<bool> {
        Ok(match v {
            VmValue::Reference(id) => match self.get(*id)? {
                HeapNode::Closure { function, .. } => self.constructable.contains(function),
                HeapNode::BoundFunction { target, .. } => self.is_constructor_value(target)?,
                HeapNode::Native(n) => super::super::js_prototype_global(n) && n != "Function",
                _ => false,
            },
            _ => false,
        })
    }
}
fn same_value(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::Number(a), VmValue::Number(b)) => {
            (a.get().is_nan() && b.get().is_nan()) || a == b
        }
        _ => a == b,
    }
}
fn array_index(key: &str) -> Option<usize> {
    key.parse::<u32>()
        .ok()
        .filter(|i| *i != u32::MAX && i.to_string() == key)
        .map(|i| i as usize)
}
