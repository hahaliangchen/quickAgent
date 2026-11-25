impl Runtime {
    /// A frame for `name` with its own environment below `parent`: parameters,
    /// hoisted vars (undefined) and top-level let/const (in their dead zone).
    fn vm_frame(
        program: &Program,
        heap: &mut super::heap::Heap,
        parent: Option<HeapId>,
        name: &str,
        args: Vec<VmValue>,
        destination: Option<String>,
    ) -> Result<Frame> {
        let function = program
            .functions
            .get(name)
            .with_context(|| format!("unknown function {name}"))?;
        // As in JavaScript: missing arguments are undefined, extra ones ignored.
        let actual_args = args.clone();
        let mut args = args;
        if let Some(index) = function.rest_parameter {
            ensure!(index + 1 == function.params.len(), "invalid rest parameter index");
            if args.len() < index { args.resize(index, VmValue::Undefined); }
            let rest = args.split_off(index);
            let rest = VmValue::Reference(heap.allocate(HeapNode::Array(rest))?);
            args.push(rest);
        }
        args.resize(function.params.len(), VmValue::Undefined);
        let mut bindings: BTreeMap<String, VmValue> = if function.parameter_scope {
            BTreeMap::new()
        } else {
            function.vars.iter().map(|name| (name.clone(), VmValue::Undefined)).collect()
        };
        bindings.extend(function.params.iter().cloned().zip(args));
        if function.arguments_object {
            let arguments = heap.allocate(HeapNode::Array(actual_args))?;
            bindings.insert("arguments".into(), VmValue::Reference(arguments));
        }
        if !function.lexical_this {bindings.insert("\0this".into(),VmValue::Undefined);bindings.insert("\0newTarget".into(),VmValue::Undefined);bindings.insert("\0home".into(),VmValue::Undefined);bindings.insert("\0activeConstructor".into(),VmValue::Undefined);}
        let lexical = if function.parameter_scope { &function.parameter_lexical } else { &function.lexical };
        let env = heap.new_environment(parent, bindings, lexical.iter().cloned().collect())?;
        Ok(Frame {
            function: name.into(),
            pc: 0,
            locals: BTreeMap::new(),
            vm_locals: Some(BTreeMap::new()),
            env: Some(env),
            destination,
            handlers: Vec::new(),
            handler_envs: Vec::new(),
            completion: None,
            executor: None,
            origin: Vec::new(),
            task_callback: false,
            constructed: None,
            diagnostic_at: None,
            work_origin: None,
            derived_env: None,
            super_env: None,
            generator: None,
            generator_function: None,
        })
    }
    /// The object a required library gives: each method a tool value, async
    /// for "<library>/promises", as with Node's fs and fs/promises.
    fn link(&mut self, library: &str, value: &Value) -> Result<VmValue> {
        let methods = value["methods"].as_object().context("a required library returns its methods")?;
        let asynchronous = library.ends_with("/promises");
        let prefix = if asynchronous { "tool:async-tool:" } else { "tool:" };
        let lib_prefix = if asynchronous { "library-async:" } else { "library:" };
        let base_library = library.strip_suffix("/promises").unwrap_or(library);
        let mut properties = Vec::with_capacity(methods.len());
        for (method, tool) in methods {
            let tool = tool.as_str().context("a library method names its tool")?;
            let native = self.heap.native(&format!("{prefix}{tool}"))?;
            properties.push((method.clone(), VmValue::Reference(native)));
        }
        if let Some(js_methods) = value["library_methods"].as_array() {
            properties.reserve(js_methods.len());
            for method in js_methods {
                let method = method.as_str().context("a library method name must be a string")?;
                if properties.iter().any(|(existing, _)| existing == method) {
                    continue;
                }
                let native = self.heap.native(&format!("{lib_prefix}{base_library}:{method}"))?;
                properties.push((method.to_owned(), VmValue::Reference(native)));
            }
        }
        let id = self.heap.allocate(HeapNode::Object(properties))?;
        self.required.insert(library.to_owned(), id);
        Ok(VmValue::Reference(id))
    }
    /// Dynamic require can still load the pure built-in modules without a
    /// host round trip. Static require calls are lowered to these same native
    /// values at compile time.
    fn builtin_module(&mut self, library: &str) -> Result<Option<VmValue>> {
        let spec = library.strip_prefix("node:").unwrap_or(library);
        let methods: &[&str] = match spec {
            "path" => &["join", "resolve", "relative", "basename", "normalize", "dirname", "extname", "isAbsolute"],
            "url" => &["parse"],
            "os" => &["homedir", "platform", "tmpdir"],
            "yaml" => &["parse", "stringify"],
            "js-yaml" => &["load", "dump"],
            _ => return Ok(None),
        };
        let mut properties = Vec::with_capacity(methods.len());
        for method in methods {
            let native = self.heap.native(&format!("{spec}.{method}"))?;
            properties.push((method.to_string(), VmValue::Reference(native)));
        }
        let id = self.heap.allocate(HeapNode::Object(properties))?;
        Ok(Some(VmValue::Reference(id)))
    }
    /// Write a call result into the caller's temporary.
    fn store(&mut self, destination: Option<String>, value: VmValue) -> Result<()> {
        if let Some(name) = destination {
            self.frames
                .last_mut()
                .context("call has no caller frame")?
                .vm_locals
                .as_mut()
                .context("missing VM locals")?
                .insert(name, value);
        }
        Ok(())
    }

    fn invoke_method(&mut self, receiver: VmValue, name: &str, args: Vec<VmValue>, destination: Option<String>, ignore_property: bool) -> Result<Option<PreparedAction>> {
        #[cfg(test)]
        if let VmValue::Reference(id)=&receiver {
            if matches!(self.heap.get(*id)?,HeapNode::Native(n) if n=="test262.assert") {
                let target=self.heap.test_native(&format!("test262.assert.{name}")).context("unknown test262 assertion")?;
                return self.invoke(target,args,destination);
            }
        }
        let property = if ignore_property { None } else { match &receiver {
            VmValue::Reference(id) if self.heap.regex_parts(*id).is_none() => self.heap.lookup_property(*id, name)?,
            _ => None,
        }};
        match property {
            Some(value) if super::vm_eval::callable(&self.heap, &value) => return self.invoke_receiver(value, receiver, args, destination),
            Some(_) => bail!("TypeError: {name} is not a function"),
            None => {}
        }
        if name=="forEach" && matches!(&receiver,VmValue::Reference(id) if matches!(self.heap.get(*id),Ok(HeapNode::Collection(c)) if !c.kind.weak())) {
            let callback=args.first().cloned().unwrap_or(VmValue::Undefined);
            ensure!(super::vm_eval::callable(&self.heap,&callback),"TypeError: collection forEach callback is not a function");
            ensure!(self.frames.len()<64,"program call depth limit");
            let this_arg=args.get(1).cloned().unwrap_or(VmValue::Undefined);
            let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$collectionForEach",vec![receiver,callback,this_arg],destination)?;
            self.push_frame(frame);return Ok(None);
        }
        if let VmValue::Reference(source) = receiver
            && matches!(self.heap.get(source)?, HeapNode::Promise { .. })
            && matches!(name, "then" | "catch")
        {
            let target = self.promises.create_heap_promise(&mut self.heap)?;
            self.async_promises.push(target);
            let (fulfilled, rejected) = if name == "catch" {
                (VmValue::Undefined, args.first().cloned().unwrap_or(VmValue::Undefined))
            } else {
                (args.first().cloned().unwrap_or(VmValue::Undefined), args.get(1).cloned().unwrap_or(VmValue::Undefined))
            };
            let id = super::promise::ContinuationId(self.next_continuation);
            self.next_continuation = self.next_continuation.checked_add(1).context("continuation id exhausted")?;
            self.reactions.insert(id, PromiseReaction { at:self.current_call_site(), work_origin:self.work_site(), fulfilled, rejected, target, origin: self.async_origin(), scope: self.named_bindings() });
            self.promises.subscribe_heap_promise(&mut self.heap, source, id)?;
            self.store(destination, VmValue::Reference(target))?;
            return Ok(None);
        }
        if let VmValue::Reference(source) = receiver
            && matches!(self.heap.get(source)?, HeapNode::Promise { .. }) && name == "finally"
        {
            let callback = args.first().cloned().unwrap_or(VmValue::Undefined);
            let frame = Self::vm_frame(&self.program, &mut self.heap, self.module_env, "$$promiseFinally", vec![VmValue::Reference(source), callback], destination)?;
            self.push_frame(frame);
            return Ok(None);
        }
        let callback_method = matches!(name, "map" | "filter" | "forEach" | "some" | "every" | "find" | "findIndex" | "findLast" | "findLastIndex" | "flatMap" | "reduce" | "reduceRight") || matches!(name, "sort" | "toSorted") && !args.is_empty();
        let is_array = matches!(&receiver, VmValue::Reference(id) if matches!(self.heap.get(*id), Ok(HeapNode::Array(_))));
        if callback_method && is_array {
            ensure!(!args.is_empty() && super::vm_eval::callable(&self.heap, &args[0]), "TypeError: callback is not a function");
            let mut call_args = vec![receiver];
            call_args.extend(args.iter().cloned());
            if matches!(name, "reduce" | "reduceRight") {
                let supplied = args.len() > 1;
                if !supplied { call_args.push(VmValue::Undefined); }
                call_args.push(VmValue::Boolean(supplied));
            }
            ensure!(self.frames.len() < 64, "program call depth limit");
            let frame = Self::vm_frame(&self.program, &mut self.heap, self.module_env, &format!("$${name}"), call_args, destination)?;
            self.push_frame(frame);
            return Ok(None);
        }
        if matches!(&receiver, VmValue::String(_)) && matches!(name, "replace" | "replaceAll") && args.get(1).is_some_and(|v| super::vm_eval::callable(&self.heap, v)) {
            ensure!(self.frames.len() < 64, "program call depth limit");
            let mut call_args = vec![receiver];
            call_args.extend(args);
            call_args.push(VmValue::Boolean(name == "replaceAll"));
            let frame = Self::vm_frame(&self.program, &mut self.heap, self.module_env, "$$replace", call_args, destination)?;
            self.push_frame(frame);
            return Ok(None);
        }
        let value = super::js_methods::call_method(&mut self.heap, receiver, name, &args)?;
        self.store(destination, value)?;
        Ok(None)
    }

    fn dir_options(&self, value: Option<&VmValue>) -> super::js_value::InspectOptions {
        let mut opts = super::js_value::InspectOptions::dir();
        let Some(VmValue::Reference(id)) = value else {
            return opts;
        };
        let num = |key: &str| -> Option<usize> {
            match self.heap.own_property(*id, key).ok().flatten() {
                Some(VmValue::Number(n)) if n.get().is_finite() && n.get() >= 0.0 => Some(n.get() as usize),
                _ => None,
            }
        };
        if let Some(depth) = num("depth") {
            opts.max_depth = depth.min(6);
        }
        if let Some(chars) = num("maxStringLength") {
            opts.max_string_chars = chars.clamp(16, 2_000);
        }
        if let Some(items) = num("maxArrayLength") {
            opts.max_items = items.clamp(1, 50);
        }
        opts
    }

    /// Preserve complete records. Exhausting capture memory ends this program;
    /// catching an ordinary JS exception cannot turn missing evidence into success.
    fn print(&mut self, method: &str, args: &[VmValue]) -> Result<()> {
        let mut line = if method == "dir" {
            let opts = self.dir_options(args.get(1));
            super::js_value::preview(&self.heap, args.first().unwrap_or(&VmValue::Undefined), opts)
        } else {
            let parts = args
                .iter()
                .map(|value| match value {
                    VmValue::Reference(_) => match self.heap.to_json(value) {
                        Ok(json) => Ok(json.to_string()),
                        Err(_) => super::js_value::to_string(&self.heap, value),
                    },
                    value => super::js_value::to_string(&self.heap, value),
                })
                .collect::<Result<Vec<_>>>()?;
            parts.join(" ")
        };
        if matches!(method, "warn" | "error") {
            line = format!("[{method}] {line}");
        }
        if self.log_bytes == 0 && !self.logs.is_empty() {
            self.log_bytes = self.logs.iter().map(|line| line.len() + std::mem::size_of::<String>()).sum();
        }
        let required = self.log_bytes.saturating_add(line.len()).saturating_add(std::mem::size_of::<String>());
        if required > CONSOLE_CAPTURE_LIMIT_BYTES {
            self.rejected_console = Some(line);
            self.fail_capacity(json!({"name":"ResourceLimitError","code":"OUTPUT_OVER_CAPACITY",
                "boundary":"console_capture","actual_bytes":required,"limit_bytes":CONSOLE_CAPTURE_LIMIT_BYTES,
                "delivered":false,"message":"Console capture exhausted program memory. This program stopped; its complete captured and rejected output is retained for archival. Reduce the requested material or process it in stages; no partial record was accepted."}));
            bail!("console capture memory exhausted");
        }
        self.log_bytes = required;
        self.logs.push(line);
        Ok(())
    }
    /// What the program has printed with console so far.
    #[cfg(test)]
    pub fn logs(&self) -> &[String] {
        &self.logs
    }
    #[cfg(test)]
    pub fn rejected_console(&self) -> Option<&str> { self.rejected_console.as_deref() }
    pub fn fail_capacity(&mut self, mut error: Value) {
        if error.get("stack").is_none() { error["stack"] = json!(self.stack()); }
        self.error = Some(error);
        // next() exposes a pending action before inspecting finished. Never
        // hand an already executed shell/tool effect to the host a second time.
        self.pending = None;
        self.clear_scheduled_callbacks();
        self.finished = true;
    }
    /// A virtual function's `@uses` variables, read now in the scope it closes
    /// over, as a JavaScript closure reads its free variables when it runs.
    fn captured(&self, function: &str, environment: Option<HeapId>) -> Result<BTreeMap<String, Value>> {
        let uses = &self.program.functions[function].uses;
        if uses.is_empty() {
            return Ok(BTreeMap::new());
        }
        let environment = environment.context("the function's scope is missing")?;
        uses.iter()
            .map(|name| {
                let value = match self.heap.lookup_binding(environment, name)? {
                    VmValue::Undefined => Value::Null,
                    value => self.heap.to_json(&value)?,
                };
                Ok((name.clone(), value))
            })
            .collect()
    }
    /// Call a function value. A closure runs in a new frame over the scope it
    /// was created in and a built-in runs here; AI functions and tools become
    /// actions for the host.
    fn invoke(
        &mut self,
        callee: VmValue,
        args: Vec<VmValue>,
        destination: Option<String>,
    ) -> Result<Option<PreparedAction>> {
        self.invoke_receiver(callee,VmValue::Undefined,args,destination)
    }
    fn get_property(&mut self,target:VmValue,key:VmValue,receiver:VmValue,destination:Option<String>)->Result<Option<PreparedAction>> {
        if matches!(target, VmValue::Null | VmValue::Undefined) {
            let kind = if matches!(target, VmValue::Null) { "null" } else { "undefined" };
            let prop = self
                .heap
                .property_key(&key)
                .ok()
                .map(|s| format!(" (reading '{s}')"))
                .unwrap_or_default();
            bail!("TypeError: Cannot read properties of {kind}{prop}");
        }
        if matches!(key,VmValue::Reference(_)){let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$keyedGet",vec![target,key,receiver],destination)?;self.push_frame(frame);return Ok(None);}
        let original_key=key.clone();
        let key=self.heap.property_key(&key)?;
        if super::heap::Heap::is_private_key(&key) {
            let VmValue::Reference(id) = target else { bail!("TypeError: private element requires an object") };
            return match self.heap.private_element(id,&key)? {
                super::heap::PrivateElement::Field(value) | super::heap::PrivateElement::Method(value) => { self.store(destination,value)?; Ok(None) }
                super::heap::PrivateElement::Accessor { get, .. } => {
                    ensure!(get != VmValue::Undefined,"TypeError: private accessor has no getter");
                    self.invoke_receiver(get,receiver,vec![],destination)
                }
            };
        }
        let owner=match target{VmValue::Reference(id)=>Some(id),_=>self.heap.primitive_prototype(&target)};
        if let Some(id)=owner && let Some((_,d))=self.heap.descriptor_owner(id,&key)? && let Some((getter,_))=d.accessor {
            if getter==VmValue::Undefined{self.store(destination,VmValue::Undefined)?;return Ok(None);}
            return self.invoke_receiver(getter,receiver,vec![],destination);
        }
        let value=super::vm_eval::member(&self.heap,&target,&original_key)?;
        self.store(destination,value)?;Ok(None)
    }
    fn invoke_receiver(&mut self, callee:VmValue, receiver:VmValue, args:Vec<VmValue>, destination:Option<String>) -> Result<Option<PreparedAction>> {self.invoke_receiver_mode(callee,receiver,args,destination,false)}
    fn invoke_receiver_mode(&mut self, mut callee:VmValue, mut receiver:VmValue, mut args:Vec<VmValue>, destination:Option<String>, constructing:bool) -> Result<Option<PreparedAction>> {
        loop {
        let node = match &callee {
            VmValue::Reference(id) => self.heap.get(*id)?.clone(),
            other => bail!(
                "TypeError: {} is not a function",
                super::js_value::to_string(&self.heap, other)?
            ),
        };
        return match node {
            HeapNode::BoundFunction{target,receiver:bound,args:prefix,..}=>{
                let mut combined=prefix;combined.extend(args);args=combined;callee=target;receiver=bound;continue;
            },
            HeapNode::Closure { function, environment } => {
                if let VmValue::Reference(id)=callee {ensure!(constructing||self.heap.class_kind(id).is_none(),"TypeError: Class constructor cannot be invoked without new");}
                let f = self
                    .program
                    .functions
                    .get(&function)
                    .with_context(|| format!("unknown function {function}"))?;
                if f.virtual_task && !f.partial_virtual {
                    ensure!(
                        args.len() == f.params.len(),
                        "{function}: expected {} arguments, got {}",
                        f.params.len(),
                        args.len()
                    );
                    let captured = self.captured(&function, Some(environment))?;
                    let captured_scope = self.capture_virtual_metadata(Some(environment), &args, false);
                    return Ok(Some(PreparedAction {
                        kind: ActionKind::Call {
                            name: function,
                            args: json_args(args, &self.heap)?,
                            captured,
                        },
                        captured_scope,
                    }));
                }
                // A named function expression refers to itself unless one of
                // its own names shadows it.
                let self_name = f.self_name.clone().filter(|name| {
                    !f.params.contains(name) && !f.parameter_lexical.contains(name)
                        && (f.parameter_scope || (!f.vars.contains(name) && !f.lexical.contains(name)))
                });
                ensure!(self.frames.len() < 64, "program call depth limit");
                let is_async = f.is_async;
                let is_generator = f.is_generator;
                let lexical_this=f.lexical_this;
                let mut frame = Self::vm_frame(
                    &self.program,
                    &mut self.heap,
                    Some(environment),
                    &function,
                    args,
                    destination,
                )?;
                if !lexical_this {
                    self.heap.define_binding(frame.env.context("function scope missing")?,"\0this",receiver)?;
                    let home=if let VmValue::Reference(id)=callee{self.heap.method_home(id).map(VmValue::Reference).unwrap_or(VmValue::Undefined)}else{VmValue::Undefined};
                    self.heap.define_binding(frame.env.unwrap(),"\0home",home)?;
                }
                if let (Some(name), Some(env)) = (self_name, frame.env) {
                    self.heap.define_binding(env, &name, callee.clone())?;
                }
                if is_generator { frame.generator_function = Some(callee.clone()); }
                if is_async && !is_generator {
                    let promise = self.promises.create_heap_promise(&mut self.heap)?;
                    self.async_promises.push(promise);
                    self.store(frame.destination.take(), VmValue::Reference(promise))?;
                    frame.completion = Some(promise);
                    frame.origin = self.async_origin();
                }
                self.push_frame(frame);
                Ok(None)
            }
            HeapNode::PromiseResolver { capability, reject } => {
                // Thenable resolution can push a helper frame. Store in the caller first.
                self.store(destination, VmValue::Undefined)?;
                self.call_resolver(capability, reject, args.into_iter().next().unwrap_or(VmValue::Undefined))?;
                Ok(None)
            }
            HeapNode::Native(name) => {
                if matches!(name.as_str(),"AsyncGenerator.next"|"AsyncGenerator.return"|"AsyncGenerator.throw") {
                    self.enqueue_async_generator(receiver,name.rsplit('.').next().unwrap(),args,destination)?;
                    return Ok(None);
                }
                if name=="AsyncGeneratorFunction" {bail!("EvalError: dynamic AsyncGeneratorFunction construction is disabled; declare async function* in source");}
                if matches!(name.as_str(),"Generator.next"|"Generator.return"|"Generator.throw") {
                    self.resume_generator(receiver, name.rsplit('.').next().unwrap(), args, destination)?;
                    return Ok(None);
                }
                if name=="GeneratorFunction" {bail!("EvalError: dynamic GeneratorFunction construction is disabled; declare function* in source");}
                if name=="Function.restricted" {bail!("TypeError: caller and arguments are unavailable on strict functions");}
                if name=="$$coercePrimitive" {let value=args[0].clone();if !matches!(value,VmValue::Reference(_)){self.store(destination,value)?;return Ok(None);}let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$toPrimitiveHint",args,destination)?;self.push_frame(frame);return Ok(None);}
                if matches!(name.as_str(),"$$looseEqual"|"$$looseNotEqual") {let (a,b)=(&args[0],&args[1]);let object=|v:&VmValue|matches!(v,VmValue::Reference(_));let null=|v:&VmValue|matches!(v,VmValue::Undefined|VmValue::Null);if object(a)!=object(b)&&!null(a)&&!null(b){let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$coercedEqual",vec![a.clone(),b.clone(),VmValue::Boolean(name=="$$looseNotEqual")],destination)?;self.push_frame(frame);return Ok(None);}let value=super::js_value::loose_equal(&self.heap,a,b)?;self.store(destination,VmValue::Boolean(if name=="$$looseNotEqual"{!value}else{value}))?;return Ok(None);}
                if matches!(name.as_str(),"String"|"Symbol"|"Symbol.for")&&matches!(args.first(),Some(VmValue::Reference(_))) {let helper=match name.as_str(){"String"=>"$$stringConvert","Symbol"=>"$$symbolConvert",_=>"$$symbolForConvert"};let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,helper,args,destination)?;self.push_frame(frame);return Ok(None);}
                if matches!(name.as_str(),"Symbol.valueOf"|"Symbol.toPrimitive"|"Symbol.toString"|"getter:Symbol.description") {let id=self.heap.unbox_symbol(&receiver)?;let value=match name.as_str(){"Symbol.toString"=>self.heap.symbol_text(id)?,"getter:Symbol.description"=>self.heap.symbol_description(id),_=>VmValue::Symbol(id)};self.store(destination,value)?;return Ok(None);}
                if name=="$$awaitSubscribe" {
                    return self.invoke_method(args[0].clone(),"then",args[1..].to_vec(),destination,true);
                }
                if name=="Iterator.identity" {self.store(destination,receiver)?;return Ok(None);}
                if name=="String.iterator" {let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$stringIterator",vec![receiver],destination)?;self.push_frame(frame);return Ok(None);}

                if name=="Function.hasInstance" {let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$ordinaryHasInstance",vec![args.first().cloned().unwrap_or(VmValue::Undefined),receiver],destination)?;self.push_frame(frame);return Ok(None);}
                if name=="$$invokeWithReceiver" {let list=super::js_functions::apply_args(&self.heap,args[2].clone())?;return self.invoke_receiver(args[0].clone(),args[1].clone(),list,destination);}
                if name=="Function.bind" {let mut a=vec![receiver];a.extend(args);let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$functionBind",a,destination)?;self.push_frame(frame);return Ok(None);}
                if name=="$$makeBound" {
                    let VmValue::Reference(target)=args[0] else{bail!("TypeError: bind target must be callable")};
                    let prefix=super::js_functions::apply_args(&self.heap,args[2].clone())?;
                    let name=super::js_value::to_string(&self.heap,&args[3])?;
                    let length=super::js_value::to_number(&self.heap,&args[4])?;
                    let bound=self.heap.allocate(HeapNode::BoundFunction{target:args[0].clone(),receiver:args[1].clone(),args:prefix,name,length:length as usize})?;
                    self.heap.raw_set_property(bound,"length".into(),args[4].clone())?;
                    self.heap.set_prototype(bound,self.heap.prototype_of(target))?;
                    self.store(destination,VmValue::Reference(bound))?;return Ok(None);
                }
                if name=="Function.apply" {let mut a=vec![receiver];a.extend(args);let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$functionApply",a,destination)?;self.push_frame(frame);return Ok(None);}
                if name=="$$getProperty" {let target=args.first().cloned().unwrap_or(VmValue::Undefined);let receiver=args.get(2).cloned().unwrap_or_else(||target.clone());return self.get_property(target,args.get(1).cloned().unwrap_or(VmValue::Undefined),receiver,destination);}
                if name=="$$constructWithTarget" {let list=super::js_functions::apply_args(&self.heap,args[1].clone())?;self.construct_as(args[0].clone(),list,destination,&Location{line:0,column:0},Some(args[2].clone()))?;return Ok(None);}
                if name=="$$constructReady" {let list=super::js_functions::apply_args(&self.heap,args[1].clone())?;self.construct_ready(args[0].clone(),list,destination,&Location{line:0,column:0},args[2].clone(),args[3].clone())?;return Ok(None);}
                if name=="$$superCallArray"||name=="$$superConstructor" {
                    let env=self.heap.constructor_environment(self.frames.last().context("super caller missing")?.env.context("super environment missing")?)?;
                    let target=self.heap.lookup_binding(env,"\0newTarget")?;
                    let active=self.heap.lookup_binding(env,"\0activeConstructor")?;
                    let VmValue::Reference(active)=active else{bail!("ReferenceError: super is only available in a derived constructor")};
                    ensure!(self.heap.class_kind(active)==Some(true),"ReferenceError: super is only available in a derived constructor");
                    let parent=args.get(1).cloned().unwrap_or_else(||self.heap.prototype_of(active).map(VmValue::Reference).unwrap_or(VmValue::Null));
                    if name=="$$superConstructor"{self.store(destination,parent)?;return Ok(None);}
                    let list=super::js_functions::apply_args(&self.heap,args[0].clone())?;
                    self.construct_as(parent,list,destination,&Location{line:0,column:0},Some(target))?;
                    self.frames.last_mut().context("super continuation missing")?.super_env=Some(env);
                    return Ok(None);
                }
                if let Some(getter)=name.strip_prefix("getter:") {
                    let value=if getter.starts_with("Map.")||getter.starts_with("Set.") {
                        let VmValue::Reference(id)=receiver else{bail!("TypeError: incompatible collection receiver")};
                        let HeapNode::Collection(c)=self.heap.get(id)? else{bail!("TypeError: incompatible collection receiver")};
                        ensure!(getter.starts_with(c.kind.name()),"TypeError: incompatible collection receiver");VmValue::Number(super::heap::JsNumber::new(c.size as f64))
                    }else{let VmValue::Reference(id)=receiver else{bail!("TypeError: incompatible RegExp receiver")};let (pattern,flags)=self.heap.regex_parts(id).context("TypeError: incompatible RegExp receiver")?;match getter.rsplit('.').next().unwrap(){"source"=>VmValue::String(pattern),"flags"=>VmValue::String(flags.clone()),key=>VmValue::Boolean(flags.contains(match key{"global"=>'g',"ignoreCase"=>'i',"multiline"=>'m',"dotAll"=>'s',"unicode"=>'u',_=>'y'}))}};
                    self.store(destination,value)?;return Ok(None);
                }
                if let Some(helper)=super::js_functions::resumable_builtin(&name) {ensure!(self.frames.len()<64,"maximum helper call depth limit");let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,helper,args,destination)?;self.push_frame(frame);return Ok(None);}

                if name == "Promise.withResolvers" {
                    let frame = Self::vm_frame(&self.program, &mut self.heap, self.module_env, "$$promiseWithResolvers", vec![receiver], destination)?;
                    self.push_frame(frame);
                    return Ok(None);
                }
                if let Some(method)=name.strip_prefix("Promise.") {
                    ensure!(Some(receiver.clone())==self.heap.intrinsic("Promise"),"TypeError: Promise static methods currently require the intrinsic Promise constructor");
                    if matches!(method,"resolve"|"reject") {
                        let value=args.into_iter().next().unwrap_or(VmValue::Undefined);
                        if method=="resolve" && matches!(&value,VmValue::Reference(id) if matches!(self.heap.get(*id),Ok(HeapNode::Promise{..}))) {self.store(destination,value)?;return Ok(None);}
                        let promise=self.promises.create_heap_promise(&mut self.heap)?;self.async_promises.push(promise);
                        self.store(destination,VmValue::Reference(promise))?;
                        if method=="reject" {self.promises.settle_heap_promise(&mut self.heap,promise,super::promise::Outcome::Rejected(value))?;}else{self.resolve_async(Some(promise),value)?;}
                        return Ok(None);
                    }
                    let helper=match method {"all"=>"$$promiseAll","allSettled"=>"$$promiseAllSettled","race"=>"$$promiseRace","any"=>"$$promiseAny",_=>bail!("TypeError: unsupported Promise static method")};
                    let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,helper,args,destination)?;self.push_frame(frame);return Ok(None);
                }
                if matches!(name.as_str(),"Function.call"|"Function.apply"|"Function.bind") {
                    ensure!(super::vm_eval::callable(&self.heap,&receiver),"TypeError: Function.{} receiver must be callable",name.rsplit('.').next().unwrap());
                    let supplied=args.first().cloned().unwrap_or(VmValue::Undefined);
                    if name=="Function.bind" {
                        let VmValue::Reference(id)=receiver.clone() else {unreachable!()};
                        let function_name=super::vm_eval::member(&self.heap,&receiver,&VmValue::String("name".into()))?;
                        let function_name=match function_name {VmValue::String(name)=>name,_=>String::new()};
                        let length=super::vm_eval::member(&self.heap,&receiver,&VmValue::String("length".into()))?;
                        let length=super::js_value::to_number(&self.heap,&length)?.max(0.0) as usize;
                        let mut prefix=args.into_iter().skip(1).collect::<Vec<_>>();
                        let length=length.saturating_sub(prefix.len());
                        let (target,bound)=match self.heap.get(id)?.clone() {
                            HeapNode::BoundFunction{target,receiver,args:old,..}=>{let mut all=old;all.append(&mut prefix);prefix=all;(target,receiver)},
                            _=>(receiver,supplied),
                        };
                        let bound=self.heap.allocate(HeapNode::BoundFunction{target,receiver:bound,args:prefix,name:format!("bound {function_name}"),length})?;
                        self.heap.set_prototype(bound,self.heap.prototype_of(id))?;
                        self.store(destination,VmValue::Reference(bound))?;
                        return Ok(None);
                    }
                    callee=receiver;receiver=supplied;
                    args=if name=="Function.apply" {super::js_functions::apply_args(&self.heap,args.get(1).cloned().unwrap_or(VmValue::Undefined))?}else{args.into_iter().skip(1).collect()};
                    continue;
                }
                if let Some(method)=name.strip_prefix("method:") {
                    let (kind,method)=method.split_once('.').context("invalid native method")?;
                    if kind=="Array" {
                        if method == "push" && let Some(length) = self.heap.try_fast_push(&receiver, &args)? {
                            self.store(destination, VmValue::Number(JsNumber::new(length as f64)))?;
                            return Ok(None);
                        }
                        let helper=match method {"sort"=>Some("$$sort"),"toSorted"=>Some("$$toSorted"),"push"=>Some("$$arrayPush"),"pop"=>Some("$$arrayPop"),"fill"=>Some("$$arrayFill"),"reverse"=>Some("$$arrayReverse"),"shift"=>Some("$$arrayShift"),"unshift"=>Some("$$arrayUnshift"),"splice"=>Some("$$arraySplice"),"toSpliced"=>Some("$$arrayToSpliced"),"with"=>Some("$$arrayWith"),"copyWithin"=>Some("$$arrayCopyWithin"),_=>None};
                        if let Some(helper)=helper{let mut a=vec![receiver];a.extend(args);let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,helper,a,destination)?;self.push_frame(frame);return Ok(None);}
                    }
                    if kind=="Object" && method=="toString" {let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$objectToString",vec![receiver],destination)?;self.push_frame(frame);return Ok(None);}
                    if kind=="Object" {
                        let value=match method {
                            "toString"=>Some(VmValue::String(self.heap.object_tag(&receiver)?)),
                            "valueOf"=>Some(self.heap.box_value(receiver.clone())?),
                            "propertyIsEnumerable"=>{let value=self.heap.box_value(receiver.clone())?;let VmValue::Reference(id)=value else{unreachable!()};let key=self.heap.property_key(args.first().unwrap_or(&VmValue::Undefined))?;Some(VmValue::Boolean(self.heap.own_descriptor(id,&key)?.is_some_and(|d|d.enumerable)))},
                            "isPrototypeOf"=>{
                                let target=args.first().cloned().unwrap_or(VmValue::Undefined);
                                let value=if let VmValue::Reference(target)=target{let base=self.heap.box_value(receiver.clone())?;let VmValue::Reference(base)=base else{unreachable!()};self.heap.has_prototype(target,base)?}else{false};
                                Some(VmValue::Boolean(value))
                            },_=>None,
                        };
                        if let Some(value)=value{self.store(destination,value)?;return Ok(None);}
                    }
                    if kind=="Function" {
                        ensure!(super::vm_eval::callable(&self.heap,&receiver),"TypeError: incompatible Function method receiver");
                        let value=VmValue::String(super::js_value::to_string(&self.heap,&receiver)?);self.store(destination,value)?;return Ok(None);
                    }
                    if let VmValue::Reference(id)=receiver.clone() && let Some(value)=self.heap.boxed_value(id).cloned(){receiver=value;}
                    if matches!(kind,"String"|"Number"|"Boolean") && matches!(method,"toString"|"valueOf") {
                        ensure!(matches!((&receiver,kind),(VmValue::String(_)|VmValue::Utf16(_),"String")|(VmValue::Number(_),"Number")|(VmValue::Boolean(_),"Boolean")),"TypeError: incompatible {kind} method receiver");
                    }
                    if kind=="String" && method=="localeCompare" || kind=="Number" && method=="toLocaleString" || kind=="Date" && matches!(method,"toLocaleString"|"toLocaleDateString"|"toLocaleTimeString") {
                        let mut values=vec![receiver];
                        let helper=if kind=="String" { "$$localeCompare" } else if kind=="Number" {
                            ensure!(matches!(values[0],VmValue::Number(_)),"TypeError: Number.toLocaleString requires a Number receiver");
                            "$$numberLocale"
                        } else { values.push(VmValue::String(method.into())); "$$dateLocale" };
                        values.extend(args);
                        let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,helper,values,destination)?;
                        self.push_frame(frame);return Ok(None);
                    }
                    if kind=="Number" && method=="toExponential" {
                        ensure!(matches!(receiver,VmValue::Number(_)),"TypeError: Number.prototype.toExponential requires a Number receiver");
                        let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$numberExponential",vec![receiver,args.first().cloned().unwrap_or(VmValue::Undefined)],destination)?;self.push_frame(frame);return Ok(None);
                    }
                    if kind=="String" {
                        ensure!(!matches!(receiver,VmValue::Null|VmValue::Undefined),"TypeError: incompatible String method receiver");
                        if !matches!(receiver,VmValue::String(_)|VmValue::Utf16(_)){receiver=VmValue::String(super::js_value::to_string(&self.heap,&receiver)?);}
                    }
                    if kind=="String" && method=="concat" {let mut call=vec![receiver];call.extend(args);let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$stringConcat",call,destination)?;self.push_frame(frame);return Ok(None);}
                    let valid=match (&receiver,kind) {
                        (VmValue::Reference(id),"Array")=>matches!(self.heap.get(*id)?,HeapNode::Array(_)),
                        (VmValue::Reference(id),"Date")=>self.heap.date_value(*id).is_some(),
                        (VmValue::Reference(id),"Promise")=>matches!(self.heap.get(*id)?,HeapNode::Promise{..}),
                        (VmValue::Reference(id),"RegExp")=>self.heap.regex_parts(*id).is_some(),
                        (VmValue::Reference(_),"Error")=>true,
                        (VmValue::String(_)|VmValue::Utf16(_),"String")|(VmValue::Number(_),"Number")|(VmValue::Boolean(_),"Boolean")=>true,
                        (_,"Object")=>true,_=>false,
                    };
                    ensure!(valid,"TypeError: incompatible {kind} method receiver");
                    if kind=="Array" && method=="toString" {
                        let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$arrayToString",vec![receiver],destination)?;self.push_frame(frame);return Ok(None);
                    }
                    if kind=="Error" {
                        let name=super::vm_eval::member(&self.heap,&receiver,&VmValue::String("name".into()))?;
                        let message=super::vm_eval::member(&self.heap,&receiver,&VmValue::String("message".into()))?;
                        let name=if name==VmValue::Undefined{"Error".into()}else{super::js_value::to_string(&self.heap,&name)?};
                        let message=if message==VmValue::Undefined{String::new()}else{super::js_value::to_string(&self.heap,&message)?};
                        let text=if name.is_empty(){message}else if message.is_empty(){name}else{format!("{name}: {message}")};self.store(destination,VmValue::String(text))?;return Ok(None);
                    }
                    return self.invoke_method(receiver,method,args,destination,true);
                }
                if let Some((class,method))=name.split_once(".prototype.") {
                    if class == "Buffer" && method == "toString" {
                        let value = super::js_methods::buffer_to_string(&self.heap, &receiver, &args)?;
                        self.store(destination, value)?;
                        return Ok(None);
                    }
                    if let VmValue::Reference(id)=&receiver {
                        let valid=match self.heap.get(*id)? {
                            HeapNode::Collection(c)=>c.kind.name()==class,
                            HeapNode::Iterator(it)=>it.tag==class,
                            _=>false,
                        };
                        ensure!(valid,"TypeError: incompatible {class} method receiver");
                    }else{bail!("TypeError: incompatible {class} method receiver");}
                    if class == "Set" && super::js_collections::set_operation(method) {
                        let other = args.into_iter().next().unwrap_or(VmValue::Undefined);
                        let frame = Self::vm_frame(&self.program, &mut self.heap, self.module_env, "$$setOperation", vec![receiver, other, VmValue::String(method.into())], destination)?;
                        self.push_frame(frame);
                        return Ok(None);
                    }
                    if method=="next" {let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$iteratorNext",vec![receiver],destination)?;self.push_frame(frame);return Ok(None);}
                    return self.invoke_method(receiver,method,args,destination,true);
                }
                if timers::is_global(&name) {
                    let value = self.timer_builtin(&name, args)?;
                    self.store(destination, value)?;
                    return Ok(None);
                }
                if name=="Function.prototype" {self.store(destination,VmValue::Undefined)?;return Ok(None);}
                if name=="Array.from" && args.len()>=2 {
                    ensure!(self.frames.len()<64,"program call depth limit");
                    let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$arrayFrom",args,destination)?;
                    self.push_frame(frame);return Ok(None);
                }
                #[cfg(test)]
                if name=="test262.assert.throws" {
                    let environment=self.module_env.context("missing test realm")?;
                    let closure=self.heap.allocate(HeapNode::Closure{function:"__test262Throws".into(),environment})?;
                    return self.invoke(VmValue::Reference(closure),args,destination);
                }
                #[cfg(test)]
                if name.starts_with("test262.") {
                    let value=super::conformance::builtin(&mut self.heap,&name,&args,self.module_env)?;
                    self.store(destination,value)?;
                    return Ok(None);
                }
                if let Some(method) = name.strip_prefix("console.") {
                    self.print(method, &args)?;
                    self.store(destination, VmValue::Undefined)?;
                    return Ok(None);
                }
                if let Some((kind, _)) = self.native_host_action(&name, &args)? {
                    return Ok(Some(kind.into()));
                }
                let value = super::js_methods::builtin(&mut self.heap, &name, &args)?;
                self.store(destination, value)?;
                Ok(None)
            }
            _ => bail!(
                "TypeError: {} is not a function",
                super::js_value::to_string(&self.heap, &callee)?
            ),
        };
        }
    }

    fn promise_constructor(&mut self,executor:VmValue,destination:Option<String>,at:&Location)->Result<()> {self.promise_constructor_with_proto(executor,destination,at,None)}
    fn promise_constructor_with_proto(&mut self,executor:VmValue,destination:Option<String>,at:&Location,prototype:Option<VmValue>)->Result<()> {
        ensure!(super::vm_eval::callable(&self.heap,&executor),"TypeError: Promise executor is not a function");
        let promise=self.promises.create_heap_promise(&mut self.heap)?;self.async_promises.push(promise);
        if let Some(VmValue::Reference(proto))=prototype {self.heap.set_prototype(promise,Some(proto))?;}
        let capability=self.heap.allocate(HeapNode::PromiseCapability{promise,resolved:false})?;
        let resolve=self.heap.allocate(HeapNode::PromiseResolver{capability,reject:false})?;
        let reject=self.heap.allocate(HeapNode::PromiseResolver{capability,reject:true})?;
        self.store(destination,VmValue::Reference(promise))?;
        let before=self.frames.len();
        match self.invoke(executor,vec![VmValue::Reference(resolve),VmValue::Reference(reject)],None) {
            Ok(None)=>{if self.frames.len()>before && self.frames.last().unwrap().completion.is_none(){self.frames.last_mut().unwrap().executor=Some(capability);}},
            result=>{let message=match result{Err(error)=>format!("{error:#}"),_=>"TypeError: host tools cannot be Promise executors".into()};let error=self.runtime_error(message,at);self.call_resolver(capability,true,error)?;},
        }
        Ok(())
    }
    fn construct(&mut self,callee:VmValue,args:Vec<VmValue>,destination:Option<String>,at:&Location)->Result<()> {
        self.construct_as(callee,args,destination,at,None)
    }
    fn construct_as(&mut self,mut callee:VmValue,mut args:Vec<VmValue>,destination:Option<String>,_at:&Location,new_target:Option<VmValue>)->Result<()> {
        let mut target=new_target.unwrap_or_else(||callee.clone());
        loop {
            let VmValue::Reference(id)=callee else{bail!("TypeError: value is not a constructor")};
            if let HeapNode::BoundFunction{target:bound,args:prefix,..}=self.heap.get(id)?.clone(){
                if target==callee{target=bound.clone();}
                let mut combined=prefix;combined.extend(args);args=combined;callee=bound;
            }else{break;}
        }
        ensure!(self.heap.is_constructor_value(&callee)?,"TypeError: value is not a constructor");
        let list=VmValue::Reference(self.heap.allocate(HeapNode::Array(args))?);
        let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$constructUsingTarget",vec![callee,list,target],destination)?;
        self.push_frame(frame);Ok(())
    }
    fn construct_ready(&mut self,callee:VmValue,args:Vec<VmValue>,destination:Option<String>,at:&Location,new_target:VmValue,prototype:VmValue)->Result<()> {
        let VmValue::Reference(id)=callee else{bail!("TypeError: value is not a constructor")};
        match self.heap.get(id)?.clone(){
            HeapNode::Closure{function,..}=>{
                ensure!(self.program.functions[&function].constructable&&!self.program.functions[&function].virtual_task,"TypeError: function is not a constructor");
                let derived=self.heap.class_kind(id)==Some(true);
                let receiver=if derived{VmValue::Undefined}else{
                    let receiver=self.heap.allocate(HeapNode::Object(vec![]))?;
                    if let VmValue::Reference(proto)=prototype{self.heap.set_prototype(receiver,Some(proto))?;}
                    VmValue::Reference(receiver)
                };
                self.invoke_receiver_mode(callee.clone(),receiver.clone(),args,destination,true)?;
                let env=self.frames.last().context("constructor frame missing")?.env.context("constructor scope missing")?;
                self.heap.define_binding(env,"\0newTarget",new_target)?;
                self.heap.define_binding(env,"\0activeConstructor",callee)?;
                if derived{self.heap.uninitialize_this(env)?;self.frames.last_mut().unwrap().derived_env=Some(env);}
                else {
                    self.frames.last_mut().unwrap().constructed=Some(receiver.clone());
                    if let Some(init) = self.heap.class_initializer(id) { self.invoke_receiver(init,receiver,vec![],None)?; }
                }
            },
            HeapNode::Native(name)=>{
                let prototype=if matches!(prototype,VmValue::Reference(_)){prototype}else{self.heap.intrinsic(&format!("{name}.prototype")).unwrap_or(VmValue::Undefined)};
                if super::js_collections::NAMES.contains(&name.as_str()){
                    let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$constructCollection",vec![VmValue::String(name),args.first().cloned().unwrap_or(VmValue::Undefined),prototype],destination)?;self.push_frame(frame);return Ok(());
                }
                if name=="Promise"{return self.promise_constructor_with_proto(args.into_iter().next().unwrap_or(VmValue::Undefined),destination,at,Some(prototype));}
                if matches!(name.as_str(),"Number"|"String"|"Boolean"){
                    let values=VmValue::Reference(self.heap.allocate(HeapNode::Array(args))?);
                    let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$boxedSubclass",vec![VmValue::String(name),values,prototype],destination)?;self.push_frame(frame);return Ok(());
                }
                let value=match name.as_str(){
                    "Symbol"=>bail!("TypeError: Symbol cannot be constructed with new"),
                    "Object" if new_target!=callee=>VmValue::Reference(self.heap.allocate(HeapNode::Object(vec![]))?),
                    "Object"=>super::js_methods::builtin(&mut self.heap,"Object",&args)?,
                    "Array"=>super::js_methods::builtin(&mut self.heap,"Array",&args)?,
                    "Date"=>{
                        // Conversion may call user code; the prelude makes it resumable.
                        let mut values=args;values.push(prototype);
                        let values=VmValue::Reference(self.heap.allocate(HeapNode::Array(values))?);let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$dateSubclass",vec![values],destination)?;self.push_frame(frame);return Ok(());
                    },
                    "RegExp"=>super::js_methods::construct_regexp(&mut self.heap,&args)?,
                    _ if super::vm_eval::ERROR_CLASSES.contains(&name.as_str())=>super::js_methods::builtin(&mut self.heap,&name,&args)?,
                    _=>bail!("TypeError: {name} is not a supported constructor"),
                };
                if let (VmValue::Reference(proto),VmValue::Reference(object))=(prototype,&value){self.heap.set_prototype(*object,Some(proto))?;}
                self.store(destination,value)?;
            },
            _=>bail!("TypeError: value is not a constructor"),
        }
        Ok(())
    }

}
