impl Runtime {
    fn stack(&self) -> Value {
        json!(self
            .frames
            .iter()
            .rev()
            .map(|f| json!({"function":f.function,"pc":f.pc.saturating_sub(1)}))
            .collect::<Vec<_>>())
    }
    fn async_origin(&self) -> Vec<HeapId> {
        let mut origin = Vec::new();
        for frame in &self.frames {
            if frame.function.starts_with("$$") { continue; }
            if !frame.origin.is_empty() { origin.clone_from(&frame.origin); }
            origin.extend(frame.env);
        }
        origin
    }

    /// The resolve/reject pair shares a single durable already-resolved flag.
    fn call_resolver(&mut self, capability: HeapId, reject: bool, value: VmValue) -> Result<()> {
        let HeapNode::PromiseCapability { promise, resolved } = self.heap.get_mut(capability)? else {
            bail!("invalid Promise capability");
        };
        if *resolved { return Ok(()); }
        *resolved = true;
        let promise = *promise;
        if reject {
            self.promises.settle_heap_promise(&mut self.heap, promise, super::promise::Outcome::Rejected(value))?;
        } else {
            self.resolve_async(Some(promise), value)?;
        }
        Ok(())
    }

    /// Resolve an async return, adopting another Promise before completing it.
    fn resolve_async(&mut self, target: Option<HeapId>, value: VmValue) -> Result<()> {
        if let VmValue::Reference(source) = value {
            if matches!(self.heap.get(source)?, HeapNode::Promise { .. }) {
                if target == Some(source) {
                    let error = self.runtime_error("TypeError: a promise cannot resolve to itself".into(), &Location { line: 0, column: 0 });
                    self.promises.settle_heap_promise(&mut self.heap, source, super::promise::Outcome::Rejected(error))?;
                    return Ok(());
                }
                // As in JavaScript, a job subscribes to the adopted Promise
                // later, so adoption takes two more turns than a plain value.
                let id = super::promise::ContinuationId(self.next_continuation);
                self.next_continuation = self.next_continuation.checked_add(1).context("continuation id exhausted")?;
                self.adopting.insert(id, target);
                self.promises.enqueue(id, super::promise::Outcome::Fulfilled(VmValue::Reference(source)));
                return Ok(());
            }
            if let Some(target) = target {
                let has_accessor = self.heap.descriptor_owner(source, "then")?
                    .is_some_and(|(_, descriptor)| descriptor.accessor.is_some());
                let has_then = self.heap.lookup_property(source, "then")?
                    .is_some_and(|then| super::vm_eval::callable(&self.heap, &then));
                if has_accessor || has_then {
                    let capability = self.heap.allocate(HeapNode::PromiseCapability { promise: target, resolved: false })?;
                    let resolve = self.heap.allocate(HeapNode::PromiseResolver { capability, reject: false })?;
                    let reject = self.heap.allocate(HeapNode::PromiseResolver { capability, reject: true })?;
                    let frame = Self::vm_frame(
                        &self.program,
                        &mut self.heap,
                        self.module_env,
                        "$$resolveAwaitValue",
                        vec![VmValue::Reference(source), VmValue::Reference(resolve), VmValue::Reference(reject)],
                        None,
                    )?;
                    self.push_frame(frame);
                    return Ok(());
                }
            }
        }
        if let Some(target) = target {
            self.promises.settle_heap_promise(&mut self.heap, target, super::promise::Outcome::Fulfilled(value))?;
        } else {
            self.root_result = Some(value);
        }
        Ok(())
    }

    /// A failure reported by a tool, AI task or the host becomes an Error
    /// object. Its original fields stay available, with a stable message and
    /// the program location.
    fn host_error(&mut self, mut value: Value, at: &Location) -> VmValue {
        if !value.is_object() {
            value = json!({"message":value});
        } else if value.get("message").is_none() {
            // Keep the original tool error while giving callers a stable
            // exception message across tool and nested-task boundaries.
            value["message"] = value
                .get("error")
                .cloned()
                .unwrap_or_else(|| json!(value.to_string()));
        }
        if !value["message"].is_string() {
            value["message"] = json!(value["message"].to_string());
        }
        if value.get("location").is_none() {
            value["location"] = json!(at);
        }
        if value.get("stack").is_none() {
            value["stack"] = self.stack();
        }
        let class = value["name"]
            .as_str()
            .filter(|name| super::vm_eval::ERROR_CLASSES.contains(name) || (cfg!(test) && *name=="Test262Error" && self.program.functions.contains_key("__test262Throws")))
            .unwrap_or("Error")
            .to_owned();
        if value.get("name").is_none() {
            value["name"] = json!(class);
        }
        match self.heap.from_json(&value) {
            Ok(VmValue::Reference(id)) => {
                self.heap.set_class(id, &class);
                VmValue::Reference(id)
            }
            _ => {
                let message = VmValue::String(value["message"].as_str().unwrap_or("error").to_owned());
                super::js_methods::new_error(&mut self.heap, &class, &message, &VmValue::Undefined)
                    .unwrap_or(message)
            }
        }
    }
    fn current_call_site(&self)->Option<Location> {
        self.frames.iter().rev().find_map(|frame| {
            if frame.function.starts_with("$$"){frame.diagnostic_at.clone()}
            else{self.program.functions.get(&frame.function)?.code.get(frame.pc.saturating_sub(1)).map(|i|i.at.clone())}
        })
    }
    /// A runtime failure such as "TypeError: ..." becomes an Error of that class.
    fn runtime_error(&mut self, message: String, at: &Location) -> VmValue {
        let callback_at=self.frames.last().filter(|f|f.function.starts_with("$$")).and_then(|_|self.frames.iter().rev().find_map(|f|f.diagnostic_at.clone()));
        let at=callback_at.as_ref().unwrap_or(at);
        let (class, text) = message
            .split_once(": ")
            .filter(|(class, _)| super::vm_eval::ERROR_CLASSES.contains(class) || (cfg!(test) && *class=="Test262Error" && self.program.functions.contains_key("__test262Throws")))
            .map(|(class, text)| (class.to_owned(), text.to_owned()))
            .unwrap_or_else(|| ("Error".to_owned(), message.clone()));
        self.host_error(json!({"name":class,"message":text}), at)
    }
    /// The JSON report of an uncaught throw, for the host and the AI.
    fn error_report(&self, value: &VmValue, at: &Location) -> Value {
        let message = super::js_value::to_string(&self.heap, value).unwrap_or_else(|_| "uncaught exception".into());
        let mut report = match self.heap.to_json(value) {
            Ok(Value::Object(object)) => Value::Object(object),
            Ok(other) => json!({"message":message.clone(),"thrown":other}),
            Err(_) => json!({"message":message.clone()}),
        };
        if !report["message"].is_string() {
            report["message"] = json!(message);
        }
        if report.get("location").is_none() {
            report["location"] = json!(at);
        }
        if report.get("stack").is_none() {
            report["stack"] = self.stack();
        }
        report
    }
    /// Unwind to the nearest catch handler with the thrown value itself.
    fn throw(&mut self, value: VmValue, at: &Location) {
        // An exception escaping a generator completes it before entering its
        // caller's catch/finally. Promise boundaries nested inside it stay local.
        let async_boundary = self.frames.iter().rposition(|f| f.completion.is_some() || f.executor.is_some()).unwrap_or(0);
        if let Some(boundary) = self.frames.iter().rposition(|f| f.generator.is_some())
            && boundary >= async_boundary
            && !self.frames[boundary..].iter().any(|f| !f.handlers.is_empty())
        {
            let id = self.frames[boundary].generator.unwrap();
            if self.is_async_generator(id) {self.record_async_generator_throw(id,&value,at);}
            self.complete_generator(id);
            self.frames.truncate(boundary);
            if self.is_async_generator(id) {
                let result=self.async_generator_complete_step(id,super::promise::Outcome::Rejected(value)).and_then(|_|self.drain_async_generator(id));
                if let Err(error)=result {self.finished=true;self.error=Some(json!({"message":format!("{error:#}")}));}
            }else{self.throw(value, at);}
            return;
        }
        // Keep the failed frames intact when nobody can catch the error. A
        // scoped AI continuation can inspect their actual locals without
        // replaying the instructions or their external effects.
        let boundary = self.frames.iter().rposition(|frame| frame.completion.is_some() || frame.executor.is_some()).unwrap_or(0);
        if !self.frames[boundary..].iter().any(|frame| !frame.handlers.is_empty()) {
            let target = self.frames.get(boundary).and_then(|frame| match (frame.executor, frame.completion) {
                (Some(capability), _) => match self.heap.get(capability) {
                    Ok(HeapNode::PromiseCapability { promise, .. }) => Some(*promise),
                    _ => None,
                },
                (None, completion) => completion,
            });
            if let Some(promise) = target {
                // The frames are dropped here, so remember the site now.
                let site = json!({"location":at,"stack":self.stack()});
                let thrown = match value { VmValue::Reference(id) => Some(id), _ => None };
                for id in [Some(promise), thrown].into_iter().flatten() {
                    self.throw_sites.entry(id).or_insert_with(|| site.clone());
                }
            }
            if let Some(capability) = self.frames.get(boundary).and_then(|frame| frame.executor) {
                self.frames.truncate(boundary);
                if let Err(error) = self.call_resolver(capability, true, value) {
                    self.finished = true;
                    self.error = Some(json!({"message":format!("{error:#}")}));
                }
                return;
            }
            if let Some(promise) = self.frames.get(boundary).and_then(|frame| frame.completion) {
                self.frames.truncate(boundary);
                if let Err(error) = self.promises.settle_heap_promise(&mut self.heap, promise, super::promise::Outcome::Rejected(value)) {
                    self.finished = true;
                    self.error = Some(json!({"message": format!("{error:#}")}));
                }
                return;
            }
            self.finished = true;
            self.error = Some(self.error_report(&value, at));
            return;
        }
        while let Some(frame) = self.frames.last_mut() {
            if let Some((target, name)) = frame.handlers.pop() {
                frame.pc = target;
                // Leave the block scopes opened inside the try block.
                if let Some(env) = frame.handler_envs.pop() {
                    frame.env = env;
                }
                frame.vm_locals.as_mut().expect("VM frame is initialized").insert(name, value);
                return;
            }
            self.frames.pop();
        }
        self.finished = true;
        self.error = Some(self.error_report(&value, at));
    }
    fn raise(&mut self, value: Value, at: &Location) {
        let error = self.host_error(value, at);
        self.throw(error, at);
    }
    fn native_host_action(&self, name: &str, args: &[VmValue]) -> Result<Option<(ActionKind, bool)>> {
        if let Some(tool) = name.strip_prefix("tool:") {
            let mut values = json_args(args.to_vec(), &self.heap)?;
            let arg = if values.is_empty() { json!({}) } else { values.remove(0) };
            ensure!(arg.is_object(), "tool arguments must be an object");
            let asynchronous = tool.starts_with("async-tool:");
            return Ok(Some((ActionKind::Tool { name: tool.to_owned(), args: arg }, asynchronous)));
        }
        let (asynchronous, rest) = if let Some(rest) = name.strip_prefix("library-async:") {
            (true, rest)
        } else if let Some(rest) = name.strip_prefix("library:") {
            (false, rest)
        } else {
            return Ok(None);
        };
        let (library, method) = rest.split_once(':').context("invalid library method reference")?;
        let args = json_args(args.to_vec(), &self.heap)?;
        let library = if asynchronous { format!("async:{library}") } else { library.to_owned() };
        Ok(Some((ActionKind::LibraryCall { library, method: method.to_owned(), args }, asynchronous)))
    }

}
