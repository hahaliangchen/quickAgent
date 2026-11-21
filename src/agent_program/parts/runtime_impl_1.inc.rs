impl Runtime {
    fn migrate_frames(&mut self) -> Result<()> {
        for frame in &mut self.frames {
            if frame.vm_locals.is_none() {
                let mut values = BTreeMap::new();
                for (name, value) in &frame.locals {
                    values.insert(name.clone(), self.heap.from_json(value)?);
                }
                frame.vm_locals = Some(values);
                frame.locals.clear();
            }
        }
        Ok(())
    }

    /// The variables an AI function sees: every frame of the call chain, each
    /// through its own scope chain, inner bindings shadowing outer ones.
    /// Temporaries and names still in their dead zone are not included.
    pub fn named_bindings(&self) -> BTreeMap<String, Value> {
        if let Some(action) = self.active_async_action.as_ref().or(self.active_reaction.as_ref()) { return action.scope.clone(); }
        let export = |value: &VmValue| {
            self.heap.to_json(value).unwrap_or_else(|error| {
                json!({"$vm_unavailable":format!("value cannot be represented as JSON: {error:#}")})
            })
        };
        let mut bindings = BTreeMap::new();
        for frame in &self.frames {
            if frame.function.starts_with("$$") { continue; }
            for env in frame.origin.iter().copied().chain(frame.env) {
                let mut chain = Vec::new();
                let mut current = Some(env);
                while let Some(id) = current {
                    if chain.contains(&id) {
                        break;
                    }
                    chain.push(id);
                    current = self.heap.environment_parent(id).ok().flatten();
                }
                for id in chain.iter().rev() {
                    for (name, value) in self.heap.initialized_bindings(*id).unwrap_or_default() {
                        if name.starts_with("$$") || name.starts_with('\0') { continue; }
                        if self.builtin_namespaces.get(&name).is_some_and(|b| value == VmValue::Reference(*b)) {
                            continue;
                        }
                        // Undefined and functions are not data, but they still
                        // shadow any outer variable of the same name.
                        if self.heap.json_absent(&value) {
                            bindings.remove(&name);
                        } else {
                            bindings.insert(name, export(&value));
                        }
                    }
                }
            }
            if frame.env.is_none() && let Some(locals) = &frame.vm_locals {
                bindings.extend(
                    locals
                        .iter()
                        .filter(|(name, _)| !is_temp(name))
                        .map(|(name, value)| (name.clone(), export(value))),
                );
            } else {
                bindings.extend(frame.locals.iter().filter(|(name, _)| !is_temp(name))
                    .map(|(name, value)| (name.clone(), value.clone())));
            }
        }
        // A completed REPL segment has no active frame, but its module scope
        // remains the durable source of bindings for the next segment.
        if self.frames.is_empty() {
            if let Some(environment) = self.module_env {
                for (name, value) in self.heap.initialized_bindings(environment).unwrap_or_default() {
                    if name.starts_with("$$") || name.starts_with('\0') { continue; }
                    if self.builtin_namespaces.get(&name).is_some_and(|b| value == VmValue::Reference(*b)) {
                        continue;
                    }
                    if self.heap.json_absent(&value) {
                        bindings.remove(&name);
                    } else {
                        bindings.insert(name, export(&value));
                    }
                }
            }
        }
        bindings
    }

    fn capture_env_chain(&self, start: HeapId) -> Vec<HeapId> {
        let mut chain = Vec::new();
        let mut current = Some(start);
        while let Some(id) = current {
            if chain.contains(&id) {
                break;
            }
            chain.push(id);
            current = self.heap.environment_parent(id).ok().flatten();
        }
        chain
    }

    fn record_scope_binding(
        &self,
        scope: &mut BTreeMap<String, Value>,
        bare_required_ids: &BTreeSet<HeapId>,
        name: &str,
        value: &VmValue,
    ) {
        const MAX_SCOPE_VALUE_BYTES: usize = 128 * 1024;
        if name.starts_with("$$") || name.starts_with('\0') || is_temp(name) {
            return;
        }
        if self.heap.json_absent(value)
            || matches!(value, VmValue::Reference(id) if bare_required_ids.contains(id) || self.builtin_namespaces.values().any(|b| b == id))
        {
            scope.remove(name);
            return;
        }
        let Ok(exported) = self.heap.to_json(value) else {
            return;
        };
        let is_context = exported.get("messages").is_some_and(Value::is_array);
        let is_resource_handle = exported.get("__ref").is_some()
            || (exported.get("path").and_then(Value::as_str).is_some()
                && exported.get("content").and_then(Value::as_str).is_some())
            || (exported.get("exit_code").is_some()
                && (exported.get("stdout").is_some() || exported.get("stderr").is_some()));
        let max_bytes = if is_resource_handle {
            4 * 1024 * 1024
        } else {
            MAX_SCOPE_VALUE_BYTES
        };
        if !is_context {
            let serialized = exported.to_string();
            if serialized.len() > max_bytes {
                let preview: String = serialized.chars().take(512).collect();
                scope.insert(
                    name.to_owned(),
                    json!({
                        "$summarized": true,
                        "bytes": serialized.len(),
                        "preview": preview,
                    }),
                );
                return;
            }
        }
        scope.insert(name.to_owned(), exported);
    }

    fn collect_libraries_in_value(
        &self,
        value: &VmValue,
        required_by_id: &BTreeMap<HeapId, String>,
        seen: &mut BTreeSet<HeapId>,
        found: &mut BTreeSet<String>,
        depth: usize,
    ) {
        let VmValue::Reference(id) = *value else { return };
        if let Some(lib) = required_by_id.get(&id) {
            if lib != "host" {
                found.insert(lib.clone());
            }
            return;
        }
        if depth >= 16 || !seen.insert(id) {
            return;
        }
        match self.heap.get(id) {
            Ok(HeapNode::Object(props)) => {
                for (_, prop) in props {
                    self.collect_libraries_in_value(prop, required_by_id, seen, found, depth + 1);
                }
            }
            Ok(HeapNode::Array(items)) => {
                for item in items {
                    self.collect_libraries_in_value(item, required_by_id, seen, found, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn capture_virtual_metadata(
        &self,
        env: Option<HeapId>,
        values: &[VmValue],
        include_active_frame: bool,
    ) -> Option<Box<ActionScope>> {
        let required_by_id: BTreeMap<HeapId, String> = self
            .required
            .iter()
            .map(|(name, id)| {
                (
                    *id,
                    name.strip_suffix("/promises").unwrap_or(name).to_owned(),
                )
            })
            .collect();
        let bare_required_ids: BTreeSet<HeapId> = self.required.values().copied().collect();
        let mut scope = BTreeMap::new();
        let mut libraries = BTreeSet::new();
        let mut seen = BTreeSet::new();

        for value in values {
            self.collect_libraries_in_value(value, &required_by_id, &mut seen, &mut libraries, 0);
        }

        let mut env_ids = Vec::new();
        if let Some(id) = env {
            for item in self.capture_env_chain(id).into_iter().rev() {
                if !env_ids.contains(&item) {
                    env_ids.push(item);
                }
            }
        }
        if include_active_frame && let Some(frame) = self.frames.last() {
            for start in frame.origin.iter().copied().chain(frame.env) {
                for item in self.capture_env_chain(start).into_iter().rev() {
                    if !env_ids.contains(&item) {
                        env_ids.push(item);
                    }
                }
            }
        }

        for id in env_ids {
            let is_module_root = Some(id) == self.module_env;
            for (name, value) in self.heap.initialized_bindings(id).unwrap_or_default() {
                self.record_scope_binding(&mut scope, &bare_required_ids, &name, &value);
                if name.starts_with("$$") || name.starts_with('\0') || is_temp(&name) {
                    continue;
                }
                let is_bare_module_require = is_module_root
                    && matches!(value, VmValue::Reference(ref_id) if bare_required_ids.contains(&ref_id));
                if !is_bare_module_require {
                    self.collect_libraries_in_value(
                        &value,
                        &required_by_id,
                        &mut seen,
                        &mut libraries,
                        0,
                    );
                }
            }
        }

        if include_active_frame && let Some(frame) = self.frames.last() {
            if frame.env.is_none()
                && let Some(locals) = &frame.vm_locals
            {
                for (name, value) in locals {
                    self.record_scope_binding(&mut scope, &bare_required_ids, name, value);
                    if !is_temp(name) {
                        self.collect_libraries_in_value(
                            value,
                            &required_by_id,
                            &mut seen,
                            &mut libraries,
                            0,
                        );
                    }
                }
            } else {
                for (name, value) in &frame.locals {
                    if !is_temp(name) {
                        scope.insert(name.clone(), value.clone());
                    }
                }
            }
        }

        let libraries: Vec<String> = libraries.into_iter().collect();
        if scope.is_empty() && libraries.is_empty() {
            None
        } else {
            Some(Box::new(ActionScope { variables: scope, libraries }))
        }
    }
    /// Conservative reachability over the remaining control flow. Include
    /// back-edges and exception handlers; never infer liveness from comments.
    fn collect_dead_bindings(&mut self) {
        fn expression(value: &Expr, names: &mut std::collections::BTreeSet<String>) {
            match value {
                Expr::Variable(name) => {
                    names.insert(name.clone());
                }
                Expr::Array(items) | Expr::Template(items) | Expr::NodeBuiltin(_, items) | Expr::RegExp(items) => {
                    for item in items {
                        expression(item, names);
                    }
                }
                Expr::Object(fields) => {
                    for (_, item) in fields {
                        expression(item, names);
                    }
                }
                Expr::Member(a, b) | Expr::Binary(_, a, b) => {
                    expression(a, names);
                    expression(b, names);
                }
                Expr::Unary(_, a) | Expr::Stringify(a) | Expr::ParseJson(a) => expression(a, names),
                Expr::Conditional(a, b, c) => {
                    expression(a, names);
                    expression(b, names);
                    expression(c, names);
                }
                Expr::StringMethod(a, _, args) => {
                    expression(a, names);
                    for arg in args {
                        expression(arg, names);
                    }
                }
                Expr::Value(_) | Expr::Utf16(_) | Expr::Undefined | Expr::Function(_) | Expr::Native(_) | Expr::Intrinsic(_) => {}
            }
        }
        for frame in &mut self.frames {
            let code = &self.program.functions[&frame.function].code;
            let mut pending = vec![frame.pc];
            pending.extend(frame.handlers.iter().map(|(target, _)| *target));
            let mut visited = std::collections::BTreeSet::new();
            let mut live = std::collections::BTreeSet::new();
            let mut dynamic_scope = false;
            while let Some(pc) = pending.pop() {
                if pc >= code.len() || !visited.insert(pc) {
                    continue;
                }
                let mut next = true;
                match &code[pc].op {
                    Op::Set(_, e) | Op::Declare(_, e) => expression(e, &mut live),
                    Op::EnterScope(_) | Op::EnterFunctionBody | Op::LeaveScope | Op::CopyScope | Op::GeneratorStart => {}
                    Op::Yield { value, return_target, delegated, .. } => {
                        expression(value, &mut live);
                        if !delegated { pending.push(*return_target); }
                    }
                    Op::SetMember { object, key, value } => { expression(object, &mut live); expression(key, &mut live); expression(value, &mut live); }
                    Op::Await { value, .. } => expression(value, &mut live),
                    Op::Return(e) | Op::Throw(e) => {
                        expression(e, &mut live);
                        next = false;
                    }
                    Op::Branch(e, target) => {
                        expression(e, &mut live);
                        pending.push(*target);
                    }
                    Op::Jump(target) => {
                        pending.push(*target);
                        next = false;
                    }
                    Op::ForNext {
                        items, index, end, ..
                    } => {
                        live.insert(items.clone());
                        live.insert(index.clone());
                        pending.push(*end);
                    }
                    Op::Catch { target, .. } => pending.push(*target),
                    Op::EndCatch => {}
                    Op::Append { target, items, .. } => {
                        live.insert(target.clone());
                        for item in items {
                            expression(item, &mut live);
                        }
                    }
                    Op::Effect(_, effect) => match effect {
                        Effect::ParallelReads(operations) => {
                            for (_, args) in operations {
                                for arg in args {
                                    expression(arg, &mut live);
                                }
                            }
                        }
                        Effect::PromiseCreate { value, .. } => expression(value, &mut live),
                        Effect::PromiseConstruct { executor } => expression(executor, &mut live),
                        Effect::Require(_) => {}
                        Effect::RequireValue(value) => expression(value, &mut live),
                        Effect::ParallelReadMap { items, args, .. } => {
                            expression(items, &mut live);
                            for arg in args {
                                expression(arg, &mut live);
                            }
                        }
                        Effect::Tool(name, args) => {
                            dynamic_scope |= name == "read_scope";
                            for arg in args {
                                expression(arg, &mut live);
                            }
                        }
                        Effect::Call(_, args) | Effect::Fork(_, args) | Effect::Join(args) => {
                            for arg in args {
                                expression(arg, &mut live);
                            }
                        }
                        Effect::Task { input: e, .. } => expression(e, &mut live),
                        Effect::Apply { callee, args } => { expression(callee, &mut live); expression(args, &mut live); },
                        Effect::Invoke(call) | Effect::Construct(call)=>{expression(&call.callee,&mut live);expression(&call.receiver,&mut live);expression(&call.args,&mut live);if let Some(label)=&call.label{expression(label,&mut live);}},
                        Effect::MethodApply { receiver, args, .. } => { expression(receiver, &mut live); expression(args, &mut live); },
                        Effect::DynamicMethodApply { receiver, name, args } => { expression(receiver, &mut live); expression(name, &mut live); expression(args, &mut live); },
                        Effect::Dynamic { callee: target, args }
                        | Effect::Method { receiver: target, args, .. } => {
                            expression(target, &mut live);
                            for arg in args {
                                expression(arg, &mut live);
                            }
                        }
                    },
                }
                if next {
                    pending.push(pc + 1);
                }
            }
            if let Some(Action {
                kind: ActionKind::Tool { name, args },
                ..
            }) = &self.pending
                && name == "read_scope"
                && let Some(name) = args["name"].as_str()
            {
                live.insert(name.to_owned());
            }
            // Source-level variables are visible to future AI scopes even
            // when deterministic code no longer references them. Only
            // compiler-generated temporaries may be garbage-collected here.
            frame.locals.retain(|name, _| {
                !name.starts_with("$temp") || live.contains(name) || dynamic_scope
            });
            if let Some(locals) = &mut frame.vm_locals {
                locals.retain(|name, _| !name.starts_with("$temp") || live.contains(name) || dynamic_scope);
            }
        }
    }
    pub fn new(program: Program, entry: &str, args: Vec<Value>) -> Result<Self> {
        Self::new_with_scope(program, entry, args, BTreeMap::new())
    }
    /// Inherited caller names become the outermost scope. The top-level flow
    /// runs in the module environment itself; any other entry gets an empty
    /// module environment above its own.
    fn new_with_scope(
        program: Program,
        entry: &str,
        args: Vec<Value>,
        inherited_scope: BTreeMap<String, Value>,
    ) -> Result<Self> {
        ensure!(program.version == VERSION, "unsupported program version");
        ensure!(program.prelude_hash == super::prelude::hash(), "program prelude changed");
        let mut heap = super::heap::Heap::default();
        super::js_functions::install(&mut heap)?;
        for (id,function) in &program.functions {heap.register_function_length(id,function.length);heap.register_constructor(id,function.constructable);heap.register_generator(id,function.is_generator,function.is_async);}
        heap.initialize_intrinsics()?;
        let inherited = if inherited_scope.is_empty() {
            None
        } else {
            let mut bindings = BTreeMap::new();
            for (name, value) in &inherited_scope {
                bindings.insert(name.clone(), heap.from_json(value)?);
            }
            Some(heap.new_environment(None, bindings, BTreeSet::new())?)
        };
        let inherited = if program.commonjs {
            let exports = heap.allocate(HeapNode::Object(Vec::new()))?;
            let module = heap.allocate(HeapNode::Object(vec![("exports".into(), VmValue::Reference(exports))]))?;
            let bindings = [("module".to_owned(), VmValue::Reference(module)), ("exports".to_owned(), VmValue::Reference(exports))];
            Some(heap.new_environment(inherited, bindings.into(), BTreeSet::new())?)
        } else {
            inherited
        };
        let args = args
            .iter()
            .map(|value| heap.from_json(value))
            .collect::<Result<Vec<_>>>()?;
        let module_entry = program.functions.get(entry).is_some_and(|f| f.module);
        // An implementation with top-level statements runs them first, in the
        // module scope its functions close over, then continues into the entry.
        let mut init_frame = None;
        let (mut frame, module_env) = if entry != super::ENTRY && program.functions.contains_key(super::MODULE_INIT) {
            let init = Self::vm_frame(&program, &mut heap, inherited, super::MODULE_INIT, Vec::new(), None)?;
            let env = init.env.context("module initialization has no scope")?;
            init_frame = Some(init);
            (Self::vm_frame(&program, &mut heap, Some(env), entry, args, None)?, env)
        } else if module_entry {
            let frame = Self::vm_frame(&program, &mut heap, inherited, entry, args, None)?;
            let env = frame.env.context("entry frame has no scope")?;
            (frame, env)
        } else {
            let module_env = heap.new_environment(inherited, BTreeMap::new(), BTreeSet::new())?;
            let frame = Self::vm_frame(&program, &mut heap, Some(module_env), entry, args, None)?;
            (frame, module_env)
        };
        // Top-level function declarations are hoisted into the module scope.
        // Nested functions have ids containing '/' and are bound where declared.
        for (id, function) in &program.functions {
            if !function.module && !id.contains('/') {
                let closure = heap.allocate(HeapNode::Closure {
                    function: id.clone(),
                    environment: module_env,
                })?;
                heap.define_binding(module_env, id, VmValue::Reference(closure))?;
            }
        }
        if program.functions[entry].is_generator {
            frame.generator_function = Some(heap.lookup_binding(module_env, entry)?);
        }
        Ok(Self {
            program,
            scoped_calls: true,
            frames: std::iter::once(frame).chain(init_frame).collect(),
            inherited_scope,
            promises: Default::default(),
            continuations: BTreeMap::new(),
            next_continuation: 0,
            timers: BTreeMap::new(),
            next_timer_id: 0,
            microtasks: BTreeMap::new(),
            #[cfg(test)]
            timer_test_time: None,
            forwarding: BTreeMap::new(),
            adopting: BTreeMap::new(),
            reactions: BTreeMap::new(),
            async_promises: Vec::new(),
            root_result: None,
            throw_sites: BTreeMap::new(),
            heap,
            module_env: Some(module_env),
            pending: None,
            async_actions: VecDeque::new(),
            active_async_action: None,
            active_reaction: None,
            active_async_batch: Vec::new(),
            inflight_async: BTreeMap::new(),
            logs: Vec::new(),
            log_bytes: 0,
            rejected_console: None,
            required: BTreeMap::new(),
            finished: false,
            result: Value::Null,
            error: None,
            fuel: INSTRUCTION_LIMIT,
            segment_limit: INSTRUCTION_LIMIT,
            total_instructions: 0,
            total_vm_steps: 0,
            #[cfg(test)]
            test_deadline_after_steps: None,
            line_instructions: BTreeMap::new(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            time_limit_ms: PROGRAM_TIME_LIMIT_MS,
            active_segment_rollback: BTreeMap::new(),
            builtin_namespaces: BTreeMap::new(),
            unbounded_instructions: false,
            segment_wall_limit_ms: 0,
            segment_started_at: None,
            loop_guard: BTreeMap::new(),
            next_segment: 0,
        })
    }
    /// Virtual calls become durable AI task scopes; ordinary calls use frames.
    pub fn task_program(program: Program, entry: &str, args: Vec<Value>) -> Result<Self> {
        let mut runtime = Self::new(program, entry, args)?;
        runtime.scoped_calls = true;
        Ok(runtime)
    }
    pub fn task_program_with_scope(
        program: Program,
        entry: &str,
        args: Vec<Value>,
        inherited_scope: BTreeMap<String, Value>,
    ) -> Result<Self> {
        let mut runtime = Self::new_with_scope(program, entry, args, inherited_scope)?;
        runtime.scoped_calls = true;
        Ok(runtime)
    }

    /// Create an empty persistent REPL frame. The first appended segment is
    /// compiled into the same module environment.
    pub fn new_repl(libraries: super::libraries::Libraries) -> Result<Self> {
        Self::new_repl_with_scope(libraries, BTreeMap::new())
    }

    /// Create a persistent REPL frame pre-populated with explicit parameters /
    /// `@uses` captured bindings (used for `@virtual` child frames; main turns
    /// pass an empty map so every user turn starts with a clean VM).
    pub fn new_repl_with_scope(
        libraries: super::libraries::Libraries,
        initial_bindings: BTreeMap<String, Value>,
    ) -> Result<Self> {
        let program = super::compiler::compile_segment(
            "",
            "__repl_bootstrap",
            libraries.clone(),
            BTreeSet::new(),
        )?;
        let mut runtime = Self::new(program, "__repl_bootstrap", Vec::new())?;
        runtime.scoped_calls = true;
        runtime.unbounded_instructions = true;
        runtime.segment_wall_limit_ms = DEFAULT_SEGMENT_WALL_LIMIT_MS;
        runtime.install_static_libraries(&libraries)?;
        if let Some(environment) = runtime.module_env {
            for (name, json_val) in initial_bindings {
                if super::libraries::identifier(&name) {
                    let vm_val = runtime.heap.from_json(&json_val)?;
                    runtime.heap.define_binding(environment, &name, vm_val)?;
                }
            }
        }
        runtime.frames.clear();
        runtime.finished = true;
        runtime.result = Value::Null;
        Ok(runtime)
    }

    /// Context-as-code starts with the host namespaces already bound in the
    /// module environment. The values are native method objects, so calls
    /// still become the ordinary permission-checked `Action::Tool` effects.
    pub fn install_static_libraries(
        &mut self,
        libraries: &super::libraries::Libraries,
    ) -> Result<()> {
        let environment = self.module_env.context("REPL module environment is missing")?;
        for (library, methods) in libraries {
            if !super::libraries::identifier(library) {
                continue;
            }
            let mut properties = methods
                .iter()
                .map(|(method, tool)| {
                    self.heap
                        .native(&format!("tool:{tool}"))
                        .map(|native| (method.clone(), VmValue::Reference(native)))
                })
                .collect::<Result<Vec<_>>>()?;
            if !methods.is_empty() && !properties.iter().any(|(existing, _)| existing == "promises") {
                let async_properties = methods
                    .iter()
                    .map(|(method, tool)| {
                        self.heap
                            .native(&format!("tool:async-tool:{tool}"))
                            .map(|native| (method.clone(), VmValue::Reference(native)))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let promises_obj = self
                    .heap
                    .allocate(super::heap::HeapNode::Object(async_properties))?;
                properties.push(("promises".to_owned(), VmValue::Reference(promises_obj)));
            }
            if library == "host" {
                for js_method in ["context", "tools"] {
                    if !properties.iter().any(|(existing, _)| existing == js_method) {
                        let native = self.heap.native(&format!("library:host:{js_method}"))?;
                        properties.push((js_method.to_owned(), VmValue::Reference(native)));
                    }
                }
                if super::libraries::allows_delegation(libraries)
                    && !properties.iter().any(|(existing, _)| existing == "delegate")
                {
                    let native = self.heap.native("library-async:host:delegate")?;
                    properties.push(("delegate".to_owned(), VmValue::Reference(native)));
                }
            }
            if let Some(&existing_obj) = self.builtin_namespaces.get(library)
                && self.heap.lookup_binding(environment, library).ok()
                    == Some(VmValue::Reference(existing_obj))
            {
                for (method, value) in properties {
                    self.heap.raw_set_property(existing_obj, method, value)?;
                }
                continue;
            }
            let object = self.heap.allocate(super::heap::HeapNode::Object(properties))?;
            if self.heap.binding_names(environment)?.iter().any(|existing| existing == library) {
                continue;
            }
            self.heap.define_binding(
                environment,
                library,
                VmValue::Reference(object),
            )?;
            self.builtin_namespaces.insert(library.clone(), object);
        }
        Ok(())
    }

    /// Compile and install one top-level code segment onto the live module
    /// environment without advancing execution yet (caller drives `next()` or
    /// `next_concurrent()`). Compilation errors leave the live VM untouched.
    pub fn prepare_segment(
        &mut self,
        source: &str,
        libraries: super::libraries::Libraries,
        is_virtual_leaf: bool,
    ) -> Result<String> {
        ensure!(!source.trim().is_empty(), "code segment must not be empty");
        ensure!(self.pending.is_none(), "cannot append while a host action is pending");
        let environment = self.module_env.context("REPL module environment is missing")?;
        let external_names = self.heap.binding_names(environment)?.into_iter().collect::<BTreeSet<_>>();
        let entry = format!("__segment_{}", self.next_segment.saturating_add(1));
        let mut segment = super::compiler::compile_segment(source, &entry, libraries, external_names)?;
        if is_virtual_leaf && segment.functions.values().any(|fn_def| fn_def.virtual_task) {
            anyhow::bail!(
                "VIRTUAL_LAYER_LIMIT: @virtual functions are single-layer terminal helpers and cannot declare nested @virtual or @module functions"
            );
        }
        let function = segment.functions.remove(&entry).context("compiled segment entry missing")?;

        let mut new_top_level_functions = Vec::new();
        for (id, mut fn_def) in segment.functions {
            if id.starts_with("$$") {
                continue;
            }
            if fn_def.virtual_task {
                for req_lib in self.required.keys() {
                    if !fn_def.leaf_libraries.contains(req_lib) {
                        fn_def.leaf_libraries.push(req_lib.clone());
                    }
                }
            }
            if !id.contains('/') && !fn_def.module {
                new_top_level_functions.push(id.clone());
            }
            self.heap.register_function_length(&id, fn_def.length);
            self.heap.register_constructor(&id, fn_def.constructable);
            self.heap.register_generator(&id, fn_def.is_generator, fn_def.is_async);
            self.program.functions.insert(id, fn_def);
        }

        let mut rollback = BTreeMap::new();
        for name in &function.vars {
            let _ = self.heap.declare_segment_binding(environment, name, false)?;
        }
        for name in &function.lexical {
            let prev = self.heap.declare_segment_binding(environment, name, true)?;
            rollback.insert(name.clone(), prev);
        }
        let _ = self.heap.declare_segment_binding(environment, "$_", false)?;
        self.heap.define_binding(environment, "\0explicit_return", VmValue::Boolean(false))?;
        self.heap.define_binding(environment, "\0has_last_expr", VmValue::Boolean(false))?;
        let sinks_arr = self.heap.allocate(super::heap::HeapNode::Array(Vec::new()))?;
        self.heap.define_binding(environment, "\0expr_sinks", VmValue::Reference(sinks_arr))?;

        // Allocate closures only for functions newly declared in this segment so
        // existing closures keep referential identity (`fn === fn`).
        for id in new_top_level_functions {
            let closure = self.heap.allocate(super::heap::HeapNode::Closure {
                function: id.clone(),
                environment,
            })?;
            self.heap.define_binding(environment, &id, super::heap::VmValue::Reference(closure))?;
            self.builtin_namespaces.remove(&id);
        }
        for name in function.vars.iter().chain(function.lexical.iter()) {
            if !matches!(
                name.as_str(),
                "$_" | "\0explicit_return" | "\0has_last_expr" | "\0expr_sinks"
            ) {
                self.builtin_namespaces.remove(name);
            }
        }

        self.program.functions.insert(entry.clone(), function);
        if !self.program.source.is_empty() {
            self.program.source.push('\n');
        }
        self.program.source.push_str(source);

        // Clean any leftover queues from an earlier aborted segment before starting.
        self.frames.clear();
        self.promises.clear_pending_jobs();
        self.timers.clear();
        self.microtasks.clear();
        self.forwarding.clear();
        self.adopting.clear();
        self.reactions.clear();
        self.async_promises.clear();
        self.throw_sites.clear();
        self.async_actions.clear();
        self.active_async_action = None;
        self.active_reaction = None;
        self.active_async_batch.clear();
        self.inflight_async.clear();
        self.pending = None;
        self.finished = false;
        self.result = Value::Null;
        self.error = None;
        self.root_result = None;
        self.rejected_console = None;
        self.refresh_instruction_segment();
        self.next_segment = self.next_segment.saturating_add(1);
        self.active_segment_rollback = rollback;
        self.frames.push(Frame {
            function: entry.clone(),
            pc: 0,
            locals: BTreeMap::new(),
            vm_locals: Some(BTreeMap::new()),
            env: Some(environment),
            destination: None,
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
        });
        Ok(entry)
    }

    /// Append and execute one top-level code segment in the live module
    /// environment. Host actions and suspensions use the ordinary Runtime ABI.
    pub fn append_segment(
        &mut self,
        source: &str,
        libraries: super::libraries::Libraries,
        allow_return: bool,
    ) -> Result<State> {
        self.prepare_segment(source, libraries, allow_return)?;
        Ok(self.next())
    }

    pub fn segment_returned_explicitly(&self) -> bool {
        self.module_env
            .and_then(|env| self.heap.lookup_binding(env, "\0explicit_return").ok())
            == Some(VmValue::Boolean(true))
    }
    /// Return values of all top-level bare query expressions evaluated in the
    /// latest segment (e.g. `ppF.slice(280,305); shellF.slice(780,825);`),
    /// omitting `undefined` results.
    pub fn segment_expr_sinks(&self) -> Vec<Value> {
        let Some(env) = self.module_env else {
            return Vec::new();
        };
        let Ok(VmValue::Reference(id)) = self.heap.lookup_binding(env, "\0expr_sinks") else {
            return Vec::new();
        };
        let Ok(super::heap::HeapNode::Array(items)) = self.heap.get(id) else {
            return Vec::new();
        };
        items
            .iter()
            .filter(|v| !self.heap.json_absent(v))
            .filter_map(|v| self.heap.to_json(v).ok())
            .collect()
    }

    fn is_user_visible_binding(&self, environment: HeapId, name: &str, value: &VmValue) -> bool {
        if name.starts_with("$$")
            || name.starts_with('\0')
            || name.starts_with("$temp")
            || matches!(name, "this" | "arguments")
        {
            return false;
        }
        if let Some(builtin_id) = self.builtin_namespaces.get(name)
            && value == &VmValue::Reference(*builtin_id)
        {
            return false;
        }
        let _ = environment;
        true
    }

    /// Compact fingerprints of live user-defined module bindings, used to split
    /// `last_step_bindings` (new/modified) from `older_locals` (unchanged).
    pub fn user_binding_fingerprints(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(environment) = self.module_env else { return out };
        for (name, value) in self.heap.initialized_bindings(environment).unwrap_or_default() {
            if name == "$_" || !self.is_user_visible_binding(environment, &name, &value) {
                continue;
            }
            let fp = match &value {
                VmValue::Reference(id) => format!("ref:{id:?}:{}", super::js_value::inspect_older_binding(&self.heap, &value)),
                _ => super::js_value::inspect_older_binding(&self.heap, &value),
            };
            out.insert(name, fp);
        }
        out
    }


    pub fn logs_len(&self) -> usize {
        self.logs.len()
    }

    pub fn logs_since(&self, start: usize) -> Vec<String> {
        self.logs.get(start..).map(|s| s.to_vec()).unwrap_or_default()
    }



}
