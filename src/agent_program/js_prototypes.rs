//! Serializable intrinsic objects and ordinary [[Prototype]] links. Public
//! property descriptors/accessors live in js_descriptors and Runtime frames.
use super::*;

impl Heap {
    pub(crate) fn register_generator(&mut self, name: &str, enabled: bool, asynchronous: bool) {
        if enabled { self.generator_functions.insert(name.into()); if asynchronous {self.async_generator_functions.insert(name.into());} }
    }
    pub(crate) fn new_generator(&mut self, callee: &VmValue, asynchronous: bool) -> Result<HeapId> {
        let proto = if let VmValue::Reference(id) = callee { self.lookup_property(*id,"prototype")? } else { None };
        let proto = match proto { Some(VmValue::Reference(id)) => id, _ => self.intrinsics[if asynchronous {"AsyncGenerator.prototype"}else{"Generator.prototype"}] };
        let id = self.allocate(HeapNode::Object(vec![]))?;
        self.prototypes.insert(id, Some(proto));
        self.generators.insert(id, super::super::runtime::Generator::initializing(asynchronous));
        Ok(id)
    }
    fn initialize_generators(&mut self) -> Result<()> {
        let aip=self.allocate(HeapNode::Object(vec![]))?;
        self.intrinsics.insert("AsyncIterator.prototype".into(),aip);
        let identity=self.native("Iterator.identity")?;
        let key=Self::symbol_storage_key(self.well_known_symbols["asyncIterator"]);
        self.define_builtin(aip,&key,VmValue::Reference(identity))?;
        for (kind,parent) in [("Generator","Iterator.prototype"),("AsyncGenerator","AsyncIterator.prototype")] {
            let prototype=format!("{kind}.prototype");
            let function_prototype=format!("{kind}Function.prototype");
            let gp=self.allocate(HeapNode::Object(vec![]))?;
            let fp=self.allocate(HeapNode::Object(vec![]))?;
            self.intrinsics.insert(prototype.clone(),gp);
            self.intrinsics.insert(function_prototype,fp);
            self.prototypes.insert(gp,Some(self.intrinsics[parent]));
            self.prototypes.insert(fp,Some(self.intrinsics["Function.prototype"]));
            let ctor=self.native(&format!("{kind}Function"))?;
            self.prototypes.insert(ctor,Some(self.intrinsics["Function"]));
            self.define_builtin(ctor,"prototype",VmValue::Reference(fp))?;
            self.define_builtin(fp,"constructor",VmValue::Reference(ctor))?;
            self.define_builtin(fp,"prototype",VmValue::Reference(gp))?;
            self.define_builtin(gp,"constructor",VmValue::Reference(fp))?;
            for (id,key) in [(fp,"prototype"),(fp,"constructor"),(gp,"constructor")] {
                self.descriptors.entry(id).or_default().insert(key.into(),PropertyMeta{writable:false,enumerable:false,configurable:true,accessor:None});
            }
            for key in ["next","return","throw"] {self.builtin_method(&prototype,key,&format!("{kind}.{key}"))?;}
            let tag=Self::symbol_storage_key(self.well_known_symbols["toStringTag"]);
            for (id,name) in [(gp,kind.into()),(fp,format!("{kind}Function"))] {
                self.define_builtin(id,&tag,VmValue::String(name))?;
                self.descriptors.entry(id).or_default().insert(tag.clone(),PropertyMeta{writable:false,enumerable:false,configurable:true,accessor:None});
            }
        }
        Ok(())
    }
    pub(crate) fn readonly_constant(&self, id: HeapId, key: &str) -> bool {
        ["Math", "Number"].iter().any(|namespace| {
            self.intrinsics.get(*namespace) == Some(&id)
                && super::super::js_methods::constant(&format!("{namespace}.{key}")).is_some()
        })
    }
    pub(crate) fn has_realm(&self) -> bool {
        self.realm_ready
    }
    pub(crate) fn intrinsic(&self, name: &str) -> Option<VmValue> {
        self.intrinsics.get(name).copied().map(VmValue::Reference)
    }
    pub(crate) fn register_constructor(&mut self, name: &str, enabled: bool) {
        if enabled {
            self.constructable.insert(name.into());
        }
    }
    pub(crate) fn boxed_value(&self, id: HeapId) -> Option<&VmValue> {
        self.boxed.get(&id)
    }
    pub(crate) fn kind_name(&self, id: HeapId) -> Result<&str> {
        Ok(match self.get(id)? {
            HeapNode::Array(_) => "Array",
            HeapNode::Collection(c) => c.kind.name(),
            HeapNode::Iterator(it) => &it.tag,
            HeapNode::Closure { .. }
            | HeapNode::BoundFunction { .. }
            | HeapNode::Native(_)
            | HeapNode::PromiseResolver { .. } => "Function",
            HeapNode::Promise { .. } => "Promise",
            _ if self.regex_parts(id).is_some() => "RegExp",
            _ => self
                .classes
                .get(&id)
                .map(String::as_str)
                .unwrap_or("Object"),
        })
    }
    pub(super) fn initialize_node(&mut self, id: HeapId) -> Result<()> {
        if !self.realm_ready {
            return Ok(());
        }
        if matches!(
            self.get(id)?,
            HeapNode::Environment { .. } | HeapNode::PromiseCapability { .. }
        ) {
            return Ok(());
        }
        let proto = self
            .intrinsics
            .get(&format!("{}.prototype", self.kind_name(id)?))
            .copied()
            .or_else(|| self.intrinsics.get("Object.prototype").copied());
        self.prototypes.entry(id).or_insert(proto);
        if self.regex_parts(id).is_some() {
            self.non_enumerable
                .entry(id)
                .or_default()
                .insert("$flow_regex".into());
        }
        if matches!(self.get(id)?,HeapNode::Closure{..}|HeapNode::BoundFunction{..}|HeapNode::Native(_)|HeapNode::PromiseResolver{..}) {
            for key in ["length","name"] {if !self.array_properties(id).iter().any(|(k,_)|k==key){let value=self.own_property(id,key)?.unwrap_or(VmValue::Undefined);self.define_builtin(id,key,value)?;}}
            if let Some(props)=self.array_properties.get_mut(&id){props.sort_by_key(|(key,_)|match key.as_str(){"length"=>0,"name"=>1,_=>2});}
        }
        let constructor = match self.get(id)? {
            HeapNode::Closure { function, .. } => self.constructable.contains(function),
            _ => false,
        };
        if constructor {
            let prototype = self.allocate(HeapNode::Object(vec![]))?;
            self.define_builtin(id, "prototype", VmValue::Reference(prototype))?;
            self.define_builtin(prototype, "constructor", VmValue::Reference(id))?;
        }
        if matches!(self.get(id)?,HeapNode::Closure{function,..} if self.generator_functions.contains(function)) {
            let asynchronous=matches!(self.get(id)?,HeapNode::Closure{function,..} if self.async_generator_functions.contains(function));
            self.prototypes.insert(id,Some(self.intrinsics[if asynchronous {"AsyncGeneratorFunction.prototype"}else{"GeneratorFunction.prototype"}]));
            if self.own_property(id,"prototype")?.is_none() {
                let prototype=self.allocate(HeapNode::Object(vec![]))?;
                self.prototypes.insert(prototype,Some(self.intrinsics[if asynchronous {"AsyncGenerator.prototype"}else{"Generator.prototype"}]));
                self.define_builtin(id,"prototype",VmValue::Reference(prototype))?;
                self.descriptors.entry(id).or_default().insert("prototype".into(),PropertyMeta{writable:true,enumerable:false,configurable:false,accessor:None});
            }
        }
        Ok(())
    }
    pub(crate) fn define_builtin(&mut self, id: HeapId, key: &str, value: VmValue) -> Result<()> {
        match self.nodes.get_mut(&id).context("unknown intrinsic")? {
            HeapNode::Object(properties) => {
                if let Some((_, old)) = properties.iter_mut().find(|(name, _)| name == key) {
                    *old = value;
                } else {
                    properties.push((key.into(), value));
                }
            }
            _ => {
                let props = self.array_properties.entry(id).or_default();
                if let Some((_, old)) = props.iter_mut().find(|(name, _)| name == key) {
                    *old = value;
                } else {
                    props.push((key.into(), value));
                }
            }
        }
        self.non_enumerable
            .entry(id)
            .or_default()
            .insert(key.into());
        Ok(())
    }
    fn builtin_method(&mut self, proto: &str, key: &str, native: &str) -> Result<()> {
        let id = self.intrinsics[proto];
        let function = self.native(native)?;
        self.define_builtin(id, key, VmValue::Reference(function))
    }
    fn builtin_getter(&mut self, proto: &str, key: &str, kind: &str) {
        let id = self.intrinsics[proto];
        self.builtin_getters
            .entry(id)
            .or_default()
            .insert(key.into(), kind.into());
        self.non_enumerable
            .entry(id)
            .or_default()
            .insert(key.into());
    }
    pub(crate) fn initialize_intrinsics(&mut self) -> Result<()> {
        if self.realm_ready {
            return Ok(());
        }
        let object = self.allocate(HeapNode::Object(vec![]))?;
        self.intrinsics.insert("Object.prototype".into(), object);
        self.prototypes.insert(object, None);
        let function = self.native("Function.prototype")?;
        self.intrinsics
            .insert("Function.prototype".into(), function);
        self.prototypes.insert(function, Some(object));
        self.define_builtin(function, "name", VmValue::String(String::new()))?;
        for class in [
            "Symbol",
            "Array",
            "String",
            "Number",
            "Boolean",
            "Date",
            "RegExp",
            "Promise",
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "Error",
            "TypeError",
            "RangeError",
            "SyntaxError",
            "ReferenceError",
            "EvalError",
            "URIError",
            "AggregateError",
            "Map Iterator",
            "Set Iterator",
            "Array Iterator",
            "String Iterator",
            "ArrayLike Iterator",
        ] {
            let node = if class == "Array" {
                HeapNode::Array(vec![])
            } else {
                HeapNode::Object(vec![])
            };
            let id = self.allocate(node)?;
            self.intrinsics.insert(format!("{class}.prototype"), id);
            self.prototypes.insert(id, Some(object));
            if matches!(class, "String" | "Number" | "Boolean") {
                let value = match class {
                    "String" => VmValue::String(String::new()),
                    "Number" => VmValue::Number(JsNumber::new(0.0)),
                    _ => VmValue::Boolean(false),
                };
                self.boxed.insert(id, value);
                self.classes.insert(id, class.into());
            }
            if class == "Date" {
                self.dates.insert(id, JsNumber::new(f64::NAN));
                self.classes.insert(id, "Date".into());
            }
        }
        for class in [
            "Object",
            "Function",
            "Symbol",
            "Array",
            "String",
            "Number",
            "Boolean",
            "Date",
            "RegExp",
            "Promise",
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "Error",
            "TypeError",
            "RangeError",
            "SyntaxError",
            "ReferenceError",
            "EvalError",
            "URIError",
            "AggregateError",
        ] {
            let ctor = self.native(class)?;
            let proto = self.intrinsics[&format!("{class}.prototype")];
            self.intrinsics.insert(class.into(), ctor);
            self.prototypes.insert(ctor, Some(function));
            self.define_builtin(ctor, "prototype", VmValue::Reference(proto))?;
            self.define_builtin(proto, "constructor", VmValue::Reference(ctor))?;
            if super::super::vm_eval::ERROR_CLASSES.contains(&class) {
                self.define_builtin(proto, "name", VmValue::String(class.into()))?;
                self.define_builtin(proto, "message", VmValue::String(String::new()))?;
                if class != "Error" {
                    self.prototypes
                        .insert(proto, Some(self.intrinsics["Error.prototype"]));
                    self.prototypes.insert(ctor, Some(self.intrinsics["Error"]));
                }
            }
        }
        for name in ["call", "apply", "bind"] {
            self.builtin_method("Function.prototype", name, &format!("Function.{name}"))?;
        }
        for (class, methods) in [
            ("Object", "toString valueOf hasOwnProperty isPrototypeOf propertyIsEnumerable"),
            ("Function", "toString"),
            ("Array", super::super::js_functions::ARRAY),
            ("String", super::super::js_functions::STRING),
            ("Date", super::super::js_functions::DATE),
            ("Number", "toFixed toExponential toString valueOf"),
            ("Boolean", "toString valueOf"),
            ("RegExp", "test exec toString"),
            ("Promise", "then catch finally"),
        ] {
            for name in methods.split_whitespace() {
                self.builtin_method(
                    &format!("{class}.prototype"),
                    name,
                    &format!("method:{class}.{name}"),
                )?;
            }
        }
        for (class, methods) in [
            ("Array", "toString"),
            ("String", "toString valueOf"),
            ("Date", "toString valueOf toJSON"),
            ("Error", "toString"),
        ] {
            for name in methods.split_whitespace() {
                self.builtin_method(
                    &format!("{class}.prototype"),
                    name,
                    &format!("method:{class}.{name}"),
                )?;
            }
        }
        for (class, methods) in [
            (
                "Map",
                "get set has delete clear keys values entries forEach",
            ),
            ("Set", "add has delete clear keys values entries forEach"),
            ("WeakMap", "get set has delete"),
            ("WeakSet", "add has delete"),
        ] {
            for name in methods.split_whitespace() {
                let actual = if class == "Set" && name == "keys" {
                    "values"
                } else {
                    name
                };
                self.builtin_method(
                    &format!("{class}.prototype"),
                    name,
                    &format!("{class}.prototype.{actual}"),
                )?;
            }
        }
        for class in [
            "Map Iterator",
            "Set Iterator",
            "Array Iterator",
            "String Iterator",
            "ArrayLike Iterator",
        ] {
            self.builtin_method(
                &format!("{class}.prototype"),
                "next",
                &format!("{class}.prototype.next"),
            )?;
        }
        for class in ["Map", "Set"] {
            self.builtin_getter(&format!("{class}.prototype"), "size", class);
        }
        for name in [
            "source",
            "flags",
            "global",
            "ignoreCase",
            "multiline",
            "dotAll",
            "unicode",
        ] {
            self.builtin_getter("RegExp.prototype", name, "RegExp");
        }
        for (namespace, methods) in [
            (
                "Object",
                "create getPrototypeOf setPrototypeOf keys values entries fromEntries assign hasOwn is freeze defineProperty defineProperties getOwnPropertyDescriptor getOwnPropertyDescriptors getOwnPropertyNames getOwnPropertySymbols preventExtensions isExtensible seal isSealed isFrozen",
            ),
            ("Reflect", "apply construct defineProperty deleteProperty get getOwnPropertyDescriptor getPrototypeOf has isExtensible ownKeys preventExtensions set setPrototypeOf"),
            ("Symbol", "for keyFor"),
            ("Array", "isArray from of"),
            ("String", "fromCharCode fromCodePoint raw"),
            (
                "Number",
                "isInteger isSafeInteger isFinite isNaN parseInt parseFloat",
            ),
            ("Date", "now parse UTC"),
            ("Promise", "resolve reject all allSettled race any"),
            (
                "Math",
                "abs floor ceil round trunc sign sqrt cbrt exp log log2 log10 sin cos tan asin acos atan pow atan2 max min hypot random",
            ),
            ("JSON", "parse stringify"),
        ] {
            if !self.intrinsics.contains_key(namespace) {
                let id = self.allocate(HeapNode::Object(vec![]))?;
                self.intrinsics.insert(namespace.into(), id);
                self.prototypes.insert(id, Some(object));
            }
            let ns = self.intrinsics[namespace];
            for method in methods.split_whitespace() {
                let value = self.native(&format!("{namespace}.{method}"))?;
                self.define_builtin(ns, method, VmValue::Reference(value))?;
            }
        }
        for (namespace, keys) in [
            ("Math", "E LN10 LN2 LOG10E LOG2E PI SQRT1_2 SQRT2"),
            (
                "Number",
                "EPSILON MAX_VALUE MIN_VALUE MAX_SAFE_INTEGER MIN_SAFE_INTEGER NaN POSITIVE_INFINITY NEGATIVE_INFINITY",
            ),
        ] {
            for key in keys.split_whitespace() {
                if let Some(value) =
                    super::super::js_methods::constant(&format!("{namespace}.{key}"))
                {
                    self.define_builtin(
                        self.intrinsics[namespace],
                        key,
                        VmValue::Number(JsNumber::new(value)),
                    )?;
                }
            }
        }
        self.initialize_symbols()?;
        self.initialize_generators()?;
        let poison=self.native("Function.restricted")?;
        self.raw_set_property(poison,"name".into(),VmValue::String(String::new()))?;
        self.prevent_extensions(poison);
        for key in ["caller","arguments"] {
            self.define_builtin(function,key,VmValue::Undefined)?;
            self.descriptors.entry(function).or_default().insert(key.into(),PropertyMeta{writable:false,enumerable:false,configurable:true,accessor:Some((VmValue::Reference(poison),VmValue::Reference(poison)))});
        }
        let getters=self.builtin_getters.clone();
        for (id,props) in getters {for (key,kind) in props {let g=self.native(&format!("getter:{kind}.{key}"))?;self.descriptors.entry(id).or_default().insert(key,PropertyMeta{writable:false,enumerable:false,configurable:true,accessor:Some((VmValue::Reference(g),VmValue::Undefined))});}}
        self.realm_ready = true;
        for id in self.nodes.keys().copied().collect::<Vec<_>>() {
            self.initialize_node(id)?;
        }
        Ok(())
    }
    pub(crate) fn prototype_of(&self, id: HeapId) -> Option<HeapId> {
        self.prototypes.get(&id).copied().flatten()
    }
    pub(crate) fn set_prototype(&mut self, id: HeapId, proto: Option<HeapId>) -> Result<()> {
        if self.prototype_of(id) == proto {
            return Ok(());
        }
        ensure!(
            self.extensible(id),
            "TypeError: Cannot change prototype of a frozen object"
        );
        ensure!(
            self.intrinsics.get("Object.prototype") != Some(&id),
            "TypeError: Object.prototype has immutable prototype"
        );
        let mut at = proto;
        let mut seen = BTreeSet::new();
        while let Some(parent) = at {
            ensure!(
                parent != id && seen.insert(parent),
                "TypeError: Cyclic prototype value"
            );
            self.get(parent)?;
            at = self.prototype_of(parent);
        }
        self.prototypes.insert(id, proto);
        Ok(())
    }
    pub(crate) fn has_prototype(&self, id: HeapId, target: HeapId) -> Result<bool> {
        let mut at = self.prototype_of(id);
        let mut seen = BTreeSet::new();
        while let Some(parent) = at {
            ensure!(seen.insert(parent), "TypeError: Cyclic prototype value");
            if parent == target {
                return Ok(true);
            }
            at = self.prototype_of(parent);
        }
        Ok(false)
    }
    pub(crate) fn lookup_property(&self, id: HeapId, key: &str) -> Result<Option<VmValue>> {
        let mut at = Some(id);
        let mut seen = BTreeSet::new();
        while let Some(owner) = at {
            ensure!(seen.insert(owner), "TypeError: Cyclic prototype value");
            if let Some(kind) = self
                .builtin_getters
                .get(&owner)
                .and_then(|props| props.get(key))
            {
                return Ok(Some(match kind.as_str() {
                    "Map" | "Set" => match self.get(id)? {
                        HeapNode::Collection(c) if c.kind.name() == kind => {
                            VmValue::Number(JsNumber::new(c.size as f64))
                        }
                        _ => bail!("TypeError: incompatible {kind} size receiver"),
                    },
                    "RegExp" => {
                        let (pattern, flags) = self
                            .regex_parts(id)
                            .context("TypeError: incompatible RegExp receiver")?;
                        match key {
                            "source" => VmValue::String(pattern),
                            "flags" => VmValue::String(flags.clone()),
                            _ => VmValue::Boolean(flags.contains(match key {
                                "global" => 'g',
                                "ignoreCase" => 'i',
                                "multiline" => 'm',
                                "dotAll" => 's',
                                _ => 'u',
                            })),
                        }
                    }
                    _ => VmValue::Undefined,
                }));
            }
            if let Some(value) = self.own_property(owner, key)? {
                return Ok(Some(value));
            }
            at = self.prototype_of(owner);
        }
        Ok(None)
    }
    pub(crate) fn has_property(&self, id: HeapId, key: &str) -> Result<bool> {
        let mut at = Some(id);
        let mut seen = BTreeSet::new();
        while let Some(owner) = at {
            ensure!(seen.insert(owner), "TypeError: Cyclic prototype value");
            if self.own_property(owner, key)?.is_some() {
                return Ok(true);
            }
            at = self.prototype_of(owner);
        }
        Ok(false)
    }
    pub(super) fn ensure_inherited_writable(&self, id: HeapId, key: &str) -> Result<()> {
        if self.own_property(id, key)?.is_some() {
            return Ok(());
        }
        let mut at = self.prototype_of(id);
        while let Some(owner) = at {
            if self.own_property(owner, key)?.is_some() {
                ensure!(
                    !self.frozen.contains(&owner)
                        && !self
                            .builtin_getters
                            .get(&owner)
                            .is_some_and(|p| p.contains_key(key)),
                    "TypeError: Cannot set read-only inherited property {key}"
                );
                break;
            }
            at = self.prototype_of(owner);
        }
        Ok(())
    }
    pub(crate) fn enumerable(&self, id: HeapId, key: &str) -> bool {
        if key=="length" && self.boxed.get(&id).is_some_and(|v|matches!(v,VmValue::String(_)|VmValue::Utf16(_))){return false;}
        if let Some(d)=self.descriptors.get(&id).and_then(|m|m.get(key)){return d.enumerable;}
        if matches!(key, "name" | "length")
            && matches!(
                self.get(id),
                Ok(HeapNode::Closure { .. }
                    | HeapNode::Native(_)
                    | HeapNode::BoundFunction { .. }
                    | HeapNode::PromiseResolver { .. })
            )
        {
            return false;
        }
        !self
            .non_enumerable
            .get(&id)
            .is_some_and(|keys| keys.contains(key))
            && !(key == "length" && matches!(self.get(id), Ok(HeapNode::Array(_))))
    }
    pub(crate) fn own_names(&self, id: HeapId) -> Result<Vec<String>> {
        let mut names = match self.get(id)? {
            HeapNode::Object(properties) => properties.iter().map(|(key, _)| key.clone()).collect(),
            HeapNode::Array(values) => {
                let mut out = (0..values.len())
                    .filter(|i| self.has_array_index(id, *i).unwrap_or(false))
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>();
                out.push("length".into());
                out
            }
            _ => vec![],
        };
        for (key, _) in self.array_properties(id) {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }
        if let Some(getters) = self.builtin_getters.get(&id) {
            for key in getters.keys() {
                if !names.contains(key) {
                    names.push(key.clone());
                }
            }
        }
        if matches!(
            self.get(id)?,
            HeapNode::Closure { .. }
                | HeapNode::Native(_)
                | HeapNode::BoundFunction { .. }
                | HeapNode::PromiseResolver { .. }
        ) {
            for key in ["name", "length"] {
                if !names.iter().any(|k| k == key) {
                    names.push(key.into());
                }
            }
        }
        if let Some(value)=self.boxed.get(&id).filter(|v|matches!(v,VmValue::String(_)|VmValue::Utf16(_))) {
            for i in 0..super::super::js_value::string_units(self,value)?.len(){let key=i.to_string();if !names.contains(&key){names.push(key);}}
            if !names.iter().any(|k|k=="length"){names.push("length".into());}
        }
        names.retain(|key|!self.deleted_properties.get(&id).is_some_and(|keys|keys.contains(key)));
        names.sort_by_key(|key| {
            let i = key
                .parse::<u32>()
                .ok()
                .filter(|i| *i != u32::MAX && i.to_string() == *key);
            (if Self::is_symbol_key(key){2}else if i.is_none(){1}else{0}, i.unwrap_or(0))
        });
        Ok(names)
    }
    pub(crate) fn box_value(&mut self, value: VmValue) -> Result<VmValue> {
        if matches!(value, VmValue::Reference(_)) {
            return Ok(value);
        }
        let kind = match &value {
            VmValue::String(_) | VmValue::Utf16(_) => "String",
            VmValue::Number(_) => "Number",
            VmValue::Boolean(_) => "Boolean",
            VmValue::Symbol(_) => "Symbol",
            _ => bail!("TypeError: Cannot convert null or undefined to object"),
        };
        let id = self.allocate(HeapNode::Object(vec![]))?;
        self.set_class(id, kind);
        self.boxed.insert(id, value);
        Ok(VmValue::Reference(id))
    }
    pub(crate) fn primitive_prototype(&self, value: &VmValue) -> Option<HeapId> {
        let kind = match value {
            VmValue::String(_) | VmValue::Utf16(_) => "String",
            VmValue::Number(_) => "Number",
            VmValue::Boolean(_) => "Boolean",
            VmValue::Symbol(_) => "Symbol",
            _ => return None,
        };
        self.intrinsics.get(&format!("{kind}.prototype")).copied()
    }
    pub(crate) fn instance_of(&self, value: &VmValue, constructor: &VmValue) -> Result<bool> {
        let mut constructor = constructor;
        while let VmValue::Reference(id) = constructor {
            if let HeapNode::BoundFunction { target, .. } = self.get(*id)? {
                constructor = target;
            } else {
                break;
            }
        }
        ensure!(
            super::super::vm_eval::callable(self, constructor),
            "TypeError: Right-hand side of instanceof is not callable"
        );
        let VmValue::Reference(value) = value else {
            return Ok(false);
        };
        let VmValue::Reference(ctor) = constructor else {
            unreachable!()
        };
        let prototype = self
            .lookup_property(*ctor, "prototype")?
            .unwrap_or(VmValue::Undefined);
        let VmValue::Reference(prototype) = prototype else {
            bail!("TypeError: Function has non-object prototype in instanceof")
        };
        self.has_prototype(*value, prototype)
    }
    pub(crate) fn object_tag(&self, value: &VmValue) -> Result<String> {
        let tag = match value {
            VmValue::Undefined => "Undefined",
            VmValue::Null => "Null",
            VmValue::String(_) | VmValue::Utf16(_) => "String",
            VmValue::Number(_) => "Number",
            VmValue::Boolean(_) => "Boolean",
            VmValue::Symbol(_) => "Object",
            VmValue::Promise(_) => "Promise",
            VmValue::Reference(id) => {
                let kind = self.kind_name(*id)?;
                if matches!(kind,"Map"|"Set"|"WeakMap"|"WeakSet"|"Promise"|"Symbol"|"Map Iterator"|"Set Iterator"|"Array Iterator"|"String Iterator"|"ArrayLike Iterator"){"Object"}
                else if super::super::vm_eval::ERROR_CLASSES.contains(&kind) {
                    "Error"
                } else {
                    kind
                }
            }
        };
        Ok(format!("[object {tag}]"))
    }
    pub(crate) fn prototype_builtin(
        &mut self,
        name: &str,
        args: &[VmValue],
    ) -> Result<Option<VmValue>> {
        let value = args.first().cloned().unwrap_or(VmValue::Undefined);
        let proto = args.get(1).cloned().unwrap_or(VmValue::Undefined);
        Ok(Some(match name {
            "Object" => {
                if matches!(value, VmValue::Null | VmValue::Undefined) {
                    VmValue::Reference(self.allocate(HeapNode::Object(vec![]))?)
                } else {
                    self.box_value(value)?
                }
            }
            "Function" => bail!("EvalError: Function constructor / dynamic code is unavailable"),
            "$$literalPrototype" => {
                if let VmValue::Reference(id) = value {
                    match proto {
                        VmValue::Reference(proto) => self.set_prototype(id, Some(proto))?,
                        VmValue::Null => self.set_prototype(id, None)?,
                        _ => {}
                    }
                }
                value
            }
            "Promise" => bail!("TypeError: Promise constructor requires new"),
            "Object.getPrototypeOf" => {
                let prototype = match &value {
                    VmValue::Reference(id) => self.prototype_of(*id),
                    VmValue::Null | VmValue::Undefined => {
                        bail!("TypeError: Cannot convert null or undefined to object")
                    }
                    _ => self.primitive_prototype(&value),
                };
                prototype.map(VmValue::Reference).unwrap_or(VmValue::Null)
            }
            "Object.setPrototypeOf" => {
                ensure!(
                    !matches!(value, VmValue::Null | VmValue::Undefined),
                    "TypeError: Cannot set prototype of null or undefined"
                );
                let proto = match proto {
                    VmValue::Null => None,
                    VmValue::Reference(id) => Some(id),
                    _ => bail!("TypeError: Prototype must be an object or null"),
                };
                if let VmValue::Reference(id) = value {
                    self.set_prototype(id, proto)?;
                }
                value
            }
            "Object.create" => {
                let proto = match value {
                    VmValue::Null => None,
                    VmValue::Reference(id) => Some(id),
                    _ => bail!("TypeError: Object.create prototype must be an object or null"),
                };
                if let Some(descriptors) = args.get(1).filter(|v| **v != VmValue::Undefined) {
                    let VmValue::Reference(id) = descriptors else {
                        bail!("TypeError: Object.create descriptors must be an object")
                    };
                    ensure!(
                        self.own_names(*id)?.is_empty(),
                        "TypeError: Object.create property descriptors are not implemented yet; create the object and assign data properties"
                    );
                }
                let id = self.allocate(HeapNode::Object(vec![]))?;
                self.set_prototype(id, proto)?;
                VmValue::Reference(id)
            }
            "$$forInKeys" => {
                let mut at = match &value {
                    VmValue::Reference(id) => Some(*id),
                    _ => None,
                };
                let mut visited = BTreeSet::new();
                let mut names = BTreeSet::new();
                let mut out = vec![];
                if at.is_none() {
                    if let VmValue::String(s) = &value {
                        for i in 0..s.chars().count() {
                            out.push(VmValue::String(i.to_string()));
                        }
                    }
                }
                while let Some(id) = at {
                    ensure!(visited.insert(id), "TypeError: Cyclic prototype value");
                    for key in self.own_names(id)? {
                        if !Self::is_symbol_key(&key) && names.insert(key.clone()) && self.enumerable(id, &key) {
                            out.push(self.key_value(&key));
                        }
                    }
                    at = self.prototype_of(id);
                }
                VmValue::Reference(self.allocate(HeapNode::Array(out))?)
            }
            _ => return Ok(None),
        }))
    }
}
