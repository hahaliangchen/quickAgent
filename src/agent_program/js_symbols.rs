//! Symbol identity and collision-free property storage keys. All user strings
//! starting with NUL are escaped, so none can alias the Symbol namespace.
use super::*;
const SYMBOL_KEY: &str = "\0symbol:";
const STRING_KEY: &str = "\0string:";

impl Heap {
    pub(crate) fn string_key(key: &str) -> String {
        if key.starts_with('\0') {
            format!("{STRING_KEY}{key}")
        } else {
            key.into()
        }
    }
    pub(crate) fn symbol_storage_key(id: u64) -> String {
        format!("{SYMBOL_KEY}{id}")
    }
    pub(crate) fn is_symbol_key(key: &str) -> bool {
        key.starts_with(SYMBOL_KEY)
    }
    pub(crate) fn property_key(&self, value: &VmValue) -> Result<String> {
        Ok(match value {
            VmValue::Symbol(id) => Self::symbol_storage_key(*id),
            _ => Self::string_key(&super::super::js_value::to_string(self, value)?),
        })
    }
    pub(crate) fn key_value(&self, key: &str) -> VmValue {
        if let Some(id) = key.strip_prefix(SYMBOL_KEY).and_then(|s| s.parse().ok()) {
            VmValue::Symbol(id)
        } else {
            VmValue::String(key.strip_prefix(STRING_KEY).unwrap_or(key).into())
        }
    }
    pub(crate) fn symbol(&mut self, description: Option<VmValue>) -> VmValue {
        self.next_symbol += 1;
        let id = self.next_symbol;
        self.symbols.insert(id, description);
        VmValue::Symbol(id)
    }
    pub(crate) fn symbol_description(&self, id: u64) -> VmValue {
        self.symbols
            .get(&id)
            .cloned()
            .flatten()
            .unwrap_or(VmValue::Undefined)
    }
    pub(crate) fn symbol_text(&self, id: u64) -> Result<VmValue> {
        let mut units = "Symbol(".encode_utf16().collect::<Vec<_>>();
        if let Some(Some(value)) = self.symbols.get(&id) {
            units.extend(super::super::js_value::string_units(self, value)?);
        }
        units.push(')' as u16);
        Ok(super::super::js_value::from_utf16(units))
    }
    pub(crate) fn unbox_symbol(&self, value: &VmValue) -> Result<u64> {
        match value {
            VmValue::Symbol(id) => Ok(*id),
            VmValue::Reference(id) => match self.boxed.get(id) {
                Some(VmValue::Symbol(id)) => Ok(*id),
                _ => bail!("TypeError: incompatible Symbol receiver"),
            },
            _ => bail!("TypeError: incompatible Symbol receiver"),
        }
    }
    pub(crate) fn initialize_symbols(&mut self) -> Result<()> {
        let symbol = self.intrinsics["Symbol"];
        for name in [
            "iterator",
            "asyncIterator",
            "hasInstance",
            "toPrimitive",
            "toStringTag",
        ] {
            let VmValue::Symbol(id) = self.symbol(Some(VmValue::String(format!("Symbol.{name}"))))
            else {
                unreachable!()
            };
            self.well_known_symbols.insert(name.into(), id);
            self.define_builtin(symbol, name, VmValue::Symbol(id))?;
            self.descriptors.entry(symbol).or_default().insert(
                name.into(),
                PropertyMeta {
                    writable: false,
                    enumerable: false,
                    configurable: false,
                    accessor: None,
                },
            );
        }
        let proto = self.intrinsics["Symbol.prototype"];
        for (key, native) in [
            ("toString", "Symbol.toString"),
            ("valueOf", "Symbol.valueOf"),
        ] {
            let f = self.native(native)?;
            self.define_builtin(proto, key, VmValue::Reference(f))?;
        }
        let getter = self.native("getter:Symbol.description")?;
        self.raw_set_property(proto, "description".into(), VmValue::Undefined)?;
        self.descriptors.entry(proto).or_default().insert(
            "description".into(),
            PropertyMeta {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: Some((VmValue::Reference(getter), VmValue::Undefined)),
            },
        );
        let primitive = Self::symbol_storage_key(self.well_known_symbols["toPrimitive"]);
        let value_of = self.native("Symbol.toPrimitive")?;
        self.define_builtin(proto, &primitive, VmValue::Reference(value_of))?;
        self.descriptors.entry(proto).or_default().insert(
            primitive,
            PropertyMeta {
                writable: false,
                enumerable: false,
                configurable: true,
                accessor: None,
            },
        );
        let has_instance = Self::symbol_storage_key(self.well_known_symbols["hasInstance"]);
        let f = self.native("Function.hasInstance")?;
        let fp = self.intrinsics["Function.prototype"];
        self.define_builtin(fp, &has_instance, VmValue::Reference(f))?;
        self.descriptors.entry(fp).or_default().insert(
            has_instance,
            PropertyMeta {
                writable: false,
                enumerable: false,
                configurable: false,
                accessor: None,
            },
        );
        let iterator_proto = self.allocate(HeapNode::Object(vec![]))?;
        self.intrinsics
            .insert("Iterator.prototype".into(), iterator_proto);
        self.prototypes
            .insert(iterator_proto, Some(self.intrinsics["Object.prototype"]));
        let iterator = Self::symbol_storage_key(self.well_known_symbols["iterator"]);
        let identity = self.native("Iterator.identity")?;
        self.define_builtin(iterator_proto, &iterator, VmValue::Reference(identity))?;
        for (class, method) in [("Array", "values"), ("Map", "entries"), ("Set", "values")] {
            let proto = self.intrinsics[&format!("{class}.prototype")];
            let value = self
                .own_property(proto, method)?
                .context("missing iterable method")?;
            self.define_builtin(proto, &iterator, value)?;
        }
        let string_iterator = self.native("String.iterator")?;
        self.define_builtin(
            self.intrinsics["String.prototype"],
            &iterator,
            VmValue::Reference(string_iterator),
        )?;
        for class in [
            "Map Iterator",
            "Set Iterator",
            "Array Iterator",
            "String Iterator",
            "ArrayLike Iterator",
        ] {
            self.prototypes.insert(
                self.intrinsics[&format!("{class}.prototype")],
                Some(iterator_proto),
            );
        }
        let tag = Self::symbol_storage_key(self.well_known_symbols["toStringTag"]);
        for class in [
            "Symbol",
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "Promise",
            "Map Iterator",
            "Set Iterator",
            "Array Iterator",
            "String Iterator",
        ] {
            let proto = self.intrinsics[&format!("{class}.prototype")];
            self.define_builtin(proto, &tag, VmValue::String(class.into()))?;
            self.descriptors.entry(proto).or_default().insert(
                tag.clone(),
                PropertyMeta {
                    writable: false,
                    enumerable: false,
                    configurable: true,
                    accessor: None,
                },
            );
        }
        for (id, name) in [
            (f, "[Symbol.hasInstance]"),
            (value_of, "[Symbol.toPrimitive]"),
            (getter, "get description"),
            (string_iterator, "[Symbol.iterator]"),
            (identity, "[Symbol.iterator]"),
        ] {
            self.raw_set_property(id, "name".into(), VmValue::String(name.into()))?;
        }
        Ok(())
    }
    pub(crate) fn symbol_builtin(
        &mut self,
        name: &str,
        args: &[VmValue],
    ) -> Result<Option<VmValue>> {
        let first = args.first().cloned().unwrap_or(VmValue::Undefined);
        Ok(Some(match name {
            "Symbol" => {
                let description = if first == VmValue::Undefined {
                    None
                } else {
                    Some(super::super::js_value::from_utf16(
                        super::super::js_value::string_units(self, &first)?,
                    ))
                };
                self.symbol(description)
            }
            "Symbol.for" => {
                let key = super::super::js_value::string_units(self, &first)?;
                if let Some((_, id)) = self.symbol_registry.iter().find(|(k, _)| *k == key) {
                    VmValue::Symbol(*id)
                } else {
                    let value = self.symbol(Some(super::super::js_value::from_utf16(key.clone())));
                    let VmValue::Symbol(id) = value else {
                        unreachable!()
                    };
                    self.symbol_registry.push((key, id));
                    value
                }
            }
            "Symbol.keyFor" => {
                let VmValue::Symbol(id) = first else {
                    bail!("TypeError: Symbol.keyFor requires a symbol")
                };
                self.symbol_registry
                    .iter()
                    .find(|(_, i)| *i == id)
                    .map(|(k, _)| super::super::js_value::from_utf16(k.clone()))
                    .unwrap_or(VmValue::Undefined)
            }
            _ => return Ok(None),
        }))
    }
}
