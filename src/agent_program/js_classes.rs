//! Serializable class identity and method [[HomeObject]] links.
use super::*;

impl Heap {
    pub(crate) fn is_private_key(key: &str) -> bool { key.starts_with("\0private:") }
    pub(crate) fn class_initializer(&self, ctor: HeapId) -> Option<VmValue> { self.class_initializers.get(&ctor).cloned() }
    pub(crate) fn private_element(&self, id: HeapId, key: &str) -> Result<PrivateElement> {
        self.private_elements.get(&id).and_then(|p|p.get(key)).cloned().context("TypeError: object does not have the required private brand")
    }
    pub(crate) fn private_set(&mut self, id: HeapId, key: &str, value: VmValue) -> Result<VmValue> {
        match self.private_element(id,key)? {
            PrivateElement::Field(_) => { self.private_elements.get_mut(&id).unwrap().insert(key.into(),PrivateElement::Field(value)); Ok(VmValue::Undefined) }
            PrivateElement::Method(_) => bail!("TypeError: private method is read-only"),
            PrivateElement::Accessor { set, .. } => { ensure!(set != VmValue::Undefined,"TypeError: private accessor has no setter"); Ok(set) }
        }
    }
    pub(crate) fn function_name_key(&self, key: &VmValue, prefix: &str) -> Result<VmValue> {
        use super::super::js_value::{from_utf16, string_units};
        let name = if let VmValue::Symbol(id) = key {
            match self.symbol_description(*id) {
                VmValue::Undefined => Vec::new(),
                value => {
                    let mut units = vec![b'[' as u16];
                    units.extend(string_units(self, &value)?);
                    units.push(b']' as u16);
                    units
                }
            }
        } else {
            string_units(self, key)?
        };
        let mut units = if prefix.is_empty() {
            Vec::new()
        } else {
            format!("{prefix} ").encode_utf16().collect()
        };
        units.extend(name);
        Ok(from_utf16(units))
    }

    pub(crate) fn class_kind(&self, id: HeapId) -> Option<bool> {
        self.class_constructors.get(&id).copied()
    }
    pub(crate) fn method_home(&self, id: HeapId) -> Option<HeapId> {
        self.method_homes.get(&id).copied()
    }
    pub(crate) fn uninitialize_this(&mut self, env: HeapId) -> Result<()> {
        let (_, tdz) = self.environment_mut(env)?;
        tdz.insert("\0this".into());
        Ok(())
    }
    pub(crate) fn initialize_this(&mut self, env: HeapId, value: VmValue) -> Result<()> {
        let owner = self.binding_owner(env, "\0this")?;
        let (_, tdz) = self.environment_mut(owner)?;
        ensure!(
            tdz.contains("\0this"),
            "ReferenceError: Super constructor may only initialize this once"
        );
        self.initialize_binding(owner, "\0this", value)
    }
    pub(crate) fn constructor_environment(&self, env: HeapId) -> Result<HeapId> {
        self.binding_owner(env, "\0newTarget")
    }
    pub(crate) fn class_builtin(
        &mut self,
        name: &str,
        args: &[VmValue],
    ) -> Result<Option<VmValue>> {
        let Some(first) = args.first() else {
            return Ok(None);
        };
        let value = match name {
            "$$privateKey" => {
                let symbol = self.symbol(Some(first.clone()));
                let VmValue::Symbol(id) = symbol else { unreachable!() };
                self.private_symbols.insert(id);
                symbol
            }
            "$$privateHas" => {
                let VmValue::Reference(id) = first else { bail!("TypeError: right operand of private in must be an object") };
                let key = self.property_key(&args[1])?;
                VmValue::Boolean(self.private_elements.get(id).is_some_and(|p| p.contains_key(&key)))
            }
            "$$privateAdd" => {
                let VmValue::Reference(id) = first else { bail!("TypeError: private element requires an object") };
                let key = self.property_key(&args[1])?;
                ensure!(Self::is_private_key(&key), "invalid private key");
                ensure!(!self.private_elements.get(id).is_some_and(|p|p.contains_key(&key)), "TypeError: private element initialized twice");
                let kind = super::super::js_value::to_string(self,&args[3])?;
                let element = match kind.as_str() {
                    "field" => PrivateElement::Field(args[2].clone()),
                    "method" => PrivateElement::Method(args[2].clone()),
                    "accessor" => PrivateElement::Accessor { get: args[2].clone(), set: args[4].clone() },
                    _ => bail!("invalid private element kind"),
                };
                self.private_elements.entry(*id).or_default().insert(key, element);
                first.clone()
            }
            "$$classInitializer" => {
                let VmValue::Reference(ctor) = first else { bail!("invalid class constructor") };
                self.class_initializers.insert(*ctor,args[1].clone());
                first.clone()
            }
            "$$privateMethod" => {
                let (VmValue::Reference(ctor),VmValue::Reference(function)) = (first,&args[1]) else { bail!("invalid private method") };
                let home = if args[3] == VmValue::Boolean(true) { *ctor } else {
                    let Some(VmValue::Reference(home)) = self.own_property(*ctor,"prototype")? else { bail!("class prototype missing") }; home
                };
                self.method_homes.insert(*function,home);
                let prefix = super::super::js_value::to_string(self,&args[4])?;
                let name = self.function_name_key(&args[2],&prefix)?;
                self.raw_set_property(*function,"name".into(),name)?;
                args[1].clone()
            }
            "$$functionName" => {
                let VmValue::Reference(id) = *first else {
                    bail!("TypeError: function name target must be callable")
                };
                let prefix = super::super::js_value::to_string(self, &args[2])?;
                let name = self.function_name_key(&args[1], &prefix)?;
                self.raw_set_property(id, "name".into(), name)?;
                first.clone()
            }
            "$$boxPrimitive" => {
                let value = self.box_value(first.clone())?;
                if let (VmValue::Reference(id), VmValue::Reference(proto)) = (&value, &args[1]) {
                    self.set_prototype(*id, Some(*proto))?;
                }
                value
            }
            "$$isDerived" => VmValue::Boolean(
                matches!(first,VmValue::Reference(id) if self.class_kind(*id)==Some(true)),
            ),
            "$$superBase" => {
                let VmValue::Reference(id) = first else {
                    bail!("ReferenceError: super has no home object")
                };
                self.prototype_of(*id)
                    .map(VmValue::Reference)
                    .unwrap_or(VmValue::Null)
            }
            "$$classBinding" => {
                let VmValue::Reference(id) = first else {
                    bail!("class binding missing")
                };
                let HeapNode::Closure { environment, .. } = self.get(*id)? else {
                    bail!("class binding missing")
                };
                let name = super::super::js_value::to_string(self, &args[1])?;
                let owner = self.binding_owner(*environment, &name)?;
                self.readonly_bindings
                    .entry(owner)
                    .or_default()
                    .insert(name);
                first.clone()
            }
            "$$methodHome" => {
                if let (VmValue::Reference(function), VmValue::Reference(home)) = (first, &args[1])
                {
                    self.method_homes.insert(*function, *home);
                }
                first.clone()
            }
            "$$classSetup" => {
                let VmValue::Reference(ctor) = *first else {
                    bail!("invalid class constructor")
                };
                let derived = args[2] == VmValue::Boolean(true);
                let ctor_parent = if derived {
                    if let VmValue::Reference(id) = args[1] {
                        Some(id)
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(parent) = ctor_parent {
                    self.set_prototype(ctor, Some(parent))?;
                }
                let VmValue::Reference(proto) = self
                    .own_property(ctor, "prototype")?
                    .context("class prototype missing")?
                else {
                    bail!("class prototype missing")
                };
                if derived {
                    self.set_prototype(
                        proto,
                        if let VmValue::Reference(p) = args[4] {
                            Some(p)
                        } else {
                            None
                        },
                    )?;
                }
                let name = self.function_name_key(&args[3], "")?;
                self.raw_set_property(ctor, "name".into(), name)?;
                self.descriptors.entry(ctor).or_default().insert(
                    "prototype".into(),
                    PropertyMeta {
                        writable: false,
                        enumerable: false,
                        configurable: false,
                        accessor: None,
                    },
                );
                self.class_constructors.insert(ctor, derived);
                self.method_homes.insert(ctor, proto);
                first.clone()
            }
            "$$classMethod" => {
                let VmValue::Reference(ctor) = *first else {
                    bail!("invalid class constructor")
                };
                let home = if args[4] == VmValue::Boolean(true) {
                    ctor
                } else {
                    let Some(VmValue::Reference(id)) = self.own_property(ctor, "prototype")? else {
                        bail!("class prototype missing")
                    };
                    id
                };
                let key = self.property_key(&args[1])?;
                let VmValue::Reference(function) = args[2] else {
                    bail!("class method missing")
                };
                let kind = super::super::js_value::to_string(self, &args[3])?;
                self.method_homes.insert(function, home);
                let function_name =
                    self.function_name_key(&args[1], if kind == "value" { "" } else { &kind })?;
                self.raw_set_property(function, "name".into(), function_name)?;
                let mut patch = vec![
                    (kind.clone(), args[2].clone()),
                    ("enumerable".into(), VmValue::Boolean(false)),
                    ("configurable".into(), VmValue::Boolean(true)),
                ];
                if kind == "value" {
                    patch.push(("writable".into(), VmValue::Boolean(true)));
                }
                let patch = self.allocate(HeapNode::Object(patch))?;
                ensure!(
                    self.define_own(home, &key, patch)?,
                    "TypeError: Cannot define class method {key}"
                );
                first.clone()
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
}
