impl Runtime {
    fn collection_failed(&mut self, error: anyhow::Error) -> State {
        self.finished = true;
        let error = json!({"message":format!("VM heap collection failed: {error:#}")});
        self.error = Some(error.clone());
        State::Failed(error)
    }
    fn run_until_suspended(&mut self) -> State {
        // Old snapshots gain a start time once. Restores and host replies do
        // not refresh this deadline or the current instruction segment.
        if self.started_at_ms==0 {self.started_at_ms=chrono::Utc::now().timestamp_millis();}
        if !self.finished && !self.unbounded_instructions && self.remaining_time().is_zero() {self.expire_time_budget();}
        if self.program.version != VERSION {
            self.pending = None;
            self.finished = true;
            let error = json!({"message":"program language version changed; original source and results remain saved","version":self.program.version,"required_version":VERSION});
            self.error = Some(error.clone());
            return State::Failed(error);
        }
        if self.program.prelude_hash != super::prelude::hash() {
            let error = json!({"message":"program prelude changed; saved state cannot be resumed"});
            self.finished = true; self.error = Some(error.clone()); return State::Failed(error);
        }
        if let Err(error) = self.migrate_frames() {
            self.finished = true;
            let error = json!({"message":format!("failed to restore VM values: {error:#}")});
            self.error = Some(error.clone());
            return State::Failed(error);
        }
        if let Some(action) = &self.pending {
            if matches!(action.kind, ActionKind::Fork { .. } | ActionKind::Join { .. }) {
                self.finished = true;
                let error = json!({"message":"saved fork/join programs are retired and cannot be resumed"});
                self.error = Some(error.clone());
                return State::Failed(error);
            }
            return State::Action(action.clone());
        }
        if self.finished {
            return self
                .error
                .as_ref()
                .map(|e| State::Failed(e.clone()))
                .unwrap_or_else(|| State::Returned(self.result.clone()));
        }
        while !self.finished {
            if self.frames.is_empty() {
                if let Some(job) = self.promises.pop_job() {
                    if let Some(callback) = self.microtasks.remove(&job.continuation) {
                        if let Err(error) = self.start_callback(callback) {
                            let at = Location { line: 0, column: 0 };
                            let error = self.runtime_error(format!("{error:#}"), &at);
                            self.throw(error, &at);
                        }
                        continue;
                    }
                    if let Some(reaction) = self.reactions.remove(&job.continuation) {
                        let at=reaction.at.clone().unwrap_or(Location{line:0,column:0});
                        let (callback, value, rejected) = match job.outcome {
                            super::promise::Outcome::Fulfilled(value) => (reaction.fulfilled, value, false),
                            super::promise::Outcome::Rejected(value) => (reaction.rejected, value, true),
                        };
                        if !super::vm_eval::callable(&self.heap, &callback) {
                            let outcome = if rejected { super::promise::Outcome::Rejected(value) } else { super::promise::Outcome::Fulfilled(value) };
                            if let Err(error) = self.promises.settle_heap_promise(&mut self.heap, reaction.target, outcome) {
                                self.finished = true; self.error = Some(json!({"message":format!("{error:#}")}));
                            }
                            continue;
                        }
                        let node = match &callback { VmValue::Reference(id) => self.heap.get(*id).cloned(), _ => unreachable!() };
                        if let Ok(HeapNode::Native(name)) = &node
                            && (name.starts_with("tool:") || name.starts_with("library:") || name.starts_with("library-async:"))
                        {
                            let reaction_args = vec![value.clone()];
                            match self.native_host_action(name, &reaction_args) {
                                Ok(Some((mut kind, asynchronous))) => {
                                    if let ActionKind::Tool { name, .. } = &mut kind
                                        && let Some(stripped) = name.strip_prefix("async-tool:")
                                    {
                                        *name = stripped.to_owned();
                                    } else if let ActionKind::LibraryCall { library, .. } = &mut kind
                                        && let Some(stripped) = library.strip_prefix("async:")
                                    {
                                        *library = stripped.to_owned();
                                    }
                                    let action = Action { id: format!("program-{}", crate::server::agent_service::uuid_like()), at: at.clone(), destination: None, kind, captured_scope: None };
                                    let pending = PendingAsyncAction { action: action.clone(), promise: reaction.target, scope: reaction.scope };
                                    if asynchronous { self.async_actions.push_back(pending); continue; }
                                    self.pending = Some(action.clone());
                                    self.active_reaction = Some(pending);
                                    return State::Action(action);
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    let error = self.runtime_error(format!("{error:#}"), &at);
                                    if let Err(error) = self.promises.settle_heap_promise(&mut self.heap, reaction.target, super::promise::Outcome::Rejected(error)) {
                                        self.finished = true; self.error = Some(json!({"message":format!("{error:#}")}));
                                    }
                                    continue;
                                }
                            }
                        }
                        // All local callbacks use normal call dispatch: strict this,
                        // class guards, home objects, native coercion and async
                        // adoption must be identical to an ordinary invocation.
                        let result = match Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$promiseReaction",vec![callback,value],None) {
                            Ok(mut frame)=>{frame.completion=Some(reaction.target);frame.origin=reaction.origin;frame.diagnostic_at=reaction.at;frame.work_origin=reaction.work_origin;self.push_frame(frame);Ok(())},
                            Err(error)=>Err(error),
                        };
                        if let Err(error) = result {
                            let value = self.runtime_error(format!("{error:#}"), &at);
                            if let Err(error) = self.promises.settle_heap_promise(&mut self.heap, reaction.target, super::promise::Outcome::Rejected(value)) {
                                self.finished = true; self.error = Some(json!({"message":format!("{error:#}")}));
                            }
                        }
                        continue;
                    }
                    if let Some(target) = self.adopting.remove(&job.continuation) {
                        let super::promise::Outcome::Fulfilled(VmValue::Reference(source)) = job.outcome else { unreachable!() };
                        let id = super::promise::ContinuationId(self.next_continuation);
                        let result = self.next_continuation.checked_add(1).context("continuation id exhausted").and_then(|next| {
                            self.next_continuation = next;
                            self.forwarding.insert(id, target);
                            self.promises.subscribe_heap_promise(&mut self.heap, source, id)
                        });
                        if let Err(error) = result { self.finished = true; self.error = Some(json!({"message":format!("{error:#}")})); }
                        continue;
                    }
                    if let Some(target) = self.forwarding.remove(&job.continuation) {
                        let result = match job.outcome {
                            super::promise::Outcome::Fulfilled(value) => self.resolve_async(target, value),
                            super::promise::Outcome::Rejected(value) => {
                                if let Some(target) = target {
                                    self.promises.settle_heap_promise(&mut self.heap, target, super::promise::Outcome::Rejected(value)).map(|_| ())
                                } else {
                                    self.error = Some(self.error_report(&value, &Location { line: 0, column: 0 }));
                                    self.finished = true;
                                    Ok(())
                                }
                            }
                        };
                        if let Err(error) = result { self.finished = true; self.error = Some(json!({"message":format!("{error:#}")})); }
                        continue;
                    }
                    let Some(coroutine) = self.continuations.remove(&job.continuation) else {
                        self.finished = true; self.error = Some(json!({"message":"missing coroutine continuation"})); continue;
                    };
                    self.frames = coroutine.frames;
                    match job.outcome {
                        super::promise::Outcome::Fulfilled(value) => {
                            if let Err(error) = self.store(Some(coroutine.destination), value) {
                                self.finished = true; self.error = Some(json!({"message":format!("{error:#}")}));
                            }
                        }
                        super::promise::Outcome::Rejected(value) => self.throw(value, &coroutine.at),
                    }
                    continue;
                }
                if let Some(pending) = self.async_actions.pop_front() {
                    let read = |action: &Action| matches!(
                        &action.kind,
                        ActionKind::Tool { name, .. }
                            if matches!(
                                name.as_str(),
                                "read_file"
                                    | "read_file_lines"
                                    | "read_file_chunk"
                                    | "search_text"
                                    | "list_dir"
                                    | "git_status"
                                    | "git_diff"
                                    | "git_log"
                            )
                    );
                    if read(&pending.action) && self.async_actions.front().is_some_and(|item| read(&item.action)) {
                        let at = pending.action.at.clone();
                        let mut batch = vec![pending];
                        while batch.len() < 32 && self.async_actions.front().is_some_and(|item| read(&item.action)) {
                            batch.push(self.async_actions.pop_front().unwrap());
                        }
                        let operations = batch.iter().map(|item| match &item.action.kind {
                            ActionKind::Tool { name, args } => (name.clone(), args.clone()), _ => unreachable!(),
                        }).collect();
                        let action = Action { id: format!("program-{}", crate::server::agent_service::uuid_like()), at, destination: None, kind: ActionKind::ParallelReads { operations }, captured_scope: None };
                        self.pending = Some(action.clone());
                        self.active_async_batch = batch;
                        return State::Action(action);
                    }
                    let action = pending.action.clone();
                    self.pending = Some(action.clone());
                    self.active_async_action = Some(pending);
                    return State::Action(action);
                }
                let unhandled = self.async_promises.iter().find_map(|id| match self.heap.get(*id) {
                    Ok(HeapNode::Promise { outcome: Some(super::promise::Outcome::Rejected(value)), handled: false, .. }) => Some((*id, value.clone())),
                    _ => None,
                });
                if let Some((promise, value)) = unhandled {
                    let thrown = match &value { VmValue::Reference(id) => Some(*id), _ => None };
                    let site = self.throw_sites.get(&promise).or_else(|| thrown.and_then(|id| self.throw_sites.get(&id))).cloned();
                    let mut report = self.error_report(&value, &Location { line: 0, column: 0 });
                    if let Some(site) = site {
                        report["location"] = site["location"].clone();
                        report["stack"] = site["stack"].clone();
                    }
                    self.error = Some(report);
                    self.finished = true;
                    continue;
                }
                match self.dispatch_due_timer() {
                    Ok(true) => continue,
                    Ok(false) => {},
                    Err(error) => {
                        let at=Location { line: 0, column: 0 };
                        let error=self.runtime_error(format!("{error:#}"), &at);
                        self.throw(error, &at);
                        continue;
                    }
                }
                if !self.timers.is_empty() || !self.inflight_async.is_empty() { return State::Waiting; }
                if let Some(value) = self.root_result.take() {
                    match self.heap.to_json(&value) {
                        Ok(value) => self.result = value,
                        Err(_error) if value == VmValue::Undefined => self.result = Value::Null,
                        Err(error) => { self.error = Some(json!({"message":format!("{error:#}")})); }
                    }
                    self.finished = true;
                } else {
                    self.finished = true;
                    self.error = Some(json!({"message":"async execution is waiting for an unresolved promise"}));
                }
                continue;
            }
            if !self.unbounded_instructions && self.fuel == 0 {
                self.fail_capacity(self.budget_error(false));
                return State::Failed(self.error.clone().unwrap());
            }
            if self.unbounded_instructions && self.segment_started_at.is_none() {
                self.segment_started_at = Some(std::time::Instant::now());
            }
            self.total_vm_steps = self.total_vm_steps.saturating_add(1);
            if self.total_vm_steps % 256 == 0 {
                if !self.unbounded_instructions && self.remaining_time().is_zero() {
                    self.expire_time_budget();
                    return State::Failed(self.error.clone().unwrap());
                }
                if self.unbounded_instructions
                    && self.segment_wall_limit_ms > 0
                    && let Some(started) = self.segment_started_at
                    && started.elapsed().as_millis() as u64 >= self.segment_wall_limit_ms
                {
                    let elapsed = started.elapsed().as_millis() as u64;
                    let err = self.infinite_loop_error(
                        "wall_timeout",
                        format!(
                            "Infinite loop or runaway computation detected: uninterrupted JS execution ran for {elapsed}ms (limit {}ms). Check hot_lines and use native handle.grep()/includes() or bound your loop.",
                            self.segment_wall_limit_ms
                        ),
                    );
                    self.fail_capacity(err);
                    return State::Failed(self.error.clone().unwrap());
                }
                if self.heap.collection_due() && let Err(error) = self.collect_garbage() {
                    return self.collection_failed(error);
                }
            }
            let (instruction, charge, backedge_key) = {
                let frame = self.frames.last_mut().unwrap();
                let cur_pc = frame.pc;
                let instruction = self.program.functions[&frame.function].code[cur_pc].clone();
                let is_backedge = match &instruction.op {
                    Op::Jump(target) | Op::Branch(_, target) => *target <= cur_pc,
                    _ => false,
                };
                // Prelude implementation steps are not user instructions.
                // One unit per entry and backward edge prices iterations and
                // still bounds a built-in loop independently of wall time.
                let charge = !frame.function.starts_with("$$") || cur_pc == 0 || is_backedge;
                let backedge_key = if self.unbounded_instructions
                    && is_backedge
                    && !frame.function.starts_with("$$")
                {
                    Some((frame.function.clone(), cur_pc))
                } else {
                    None
                };
                frame.pc += 1;
                (instruction, charge, backedge_key)
            };
            if let Some(key) = backedge_key {
                let hits = {
                    let entry = self.loop_guard.entry(key.clone()).or_default();
                    entry.hits = entry.hits.saturating_add(1);
                    entry.hits
                };
                if hits >= 1_000 && hits % 1_000 == 0 {
                    let fp = self.loop_state_fingerprint();
                    let stuck = {
                        let entry = self.loop_guard.entry(key).or_default();
                        if entry.last_fingerprint == Some(fp) {
                            entry.unchanged_samples = entry.unchanged_samples.saturating_add(1);
                            entry.unchanged_samples >= 3
                        } else {
                            entry.last_fingerprint = Some(fp);
                            entry.unchanged_samples = 0;
                            false
                        }
                    };
                    if stuck {
                        let err = self.infinite_loop_error(
                            "state_cycle",
                            format!(
                                "Infinite loop detected at line {}: loop executed {} iterations with identical local/heap state (no progress).",
                                instruction.at.line, hits
                            ),
                        );
                        self.fail_capacity(err);
                        return State::Failed(self.error.clone().unwrap());
                    }
                }
            }
            let at = instruction.at.clone();
            let work_site = self.work_site();
            if charge {
                if !self.unbounded_instructions {
                    self.fuel -= 1;
                }
                self.total_instructions = self.total_instructions.saturating_add(1);
                if let Some((function, location)) = &work_site {
                    *self.line_instructions.entry(function.clone()).or_default().entry(location.line).or_default() += 1;
                }
            }
            let outcome = (|| -> Result<Option<Action>> {
                match instruction.op {
                    Op::Set(name, e) => {
                        let frame = self.frames.last().unwrap();
                        let value = eval(&e, frame.locals()?, &mut self.heap)?;
                        if is_temp(&name) {
                            self.frames.last_mut().unwrap().vm_locals.as_mut().unwrap().insert(name, value);
                        } else {
                            let env = self.frames.last().unwrap().env.context("missing scope")?;
                            self.heap.assign_binding(env, &name, value)?;
                        }
                    }
                    Op::Declare(name, e) => {
                        let frame = self.frames.last().unwrap();
                        let env = frame.env.context("missing scope")?;
                        let value = eval(&e, frame.locals()?, &mut self.heap)?;
                        self.heap.initialize_binding(env, &name, value)?;
                    }
                    Op::EnterScope(names) => {
                        let frame = self.frames.last_mut().unwrap();
                        frame.env = Some(self.heap.new_environment(
                            frame.env,
                            BTreeMap::new(),
                            names.into_iter().collect(),
                        )?);
                    }
                    Op::EnterFunctionBody => {
                        let frame = self.frames.last().unwrap();
                        let parent = frame.env.context("missing parameter scope")?;
                        let function = &self.program.functions[&frame.function];
                        ensure!(function.parameter_scope, "unexpected function body scope");
                        let mut bindings = BTreeMap::new();
                        for name in &function.vars {
                            let value = if function.parameter_lexical.contains(name) {
                                self.heap.lookup_binding(parent, name)?
                            } else { VmValue::Undefined };
                            bindings.insert(name.clone(), value);
                        }
                        let env = self.heap.new_environment(Some(parent), bindings, function.lexical.iter().cloned().collect())?;
                        self.frames.last_mut().unwrap().env = Some(env);
                    }
                    Op::LeaveScope => {
                        let frame = self.frames.last_mut().unwrap();
                        let env = frame.env.context("missing scope")?;
                        frame.env = self.heap.environment_parent(env)?;
                    }
                    Op::CopyScope => {
                        let frame = self.frames.last_mut().unwrap();
                        let env = frame.env.context("missing scope")?;
                        frame.env = Some(self.heap.copy_environment(env)?);
                    }
                    Op::SetMember { object, key, value } => {
                        let locals = self.frames.last().unwrap().locals()?;
                        let object = eval(&object, locals, &mut self.heap)?;
                        let key = eval(&key, locals, &mut self.heap)?;
                        let value = eval(&value, locals, &mut self.heap)?;
                        if self.heap.try_fast_set(&object, &key, &value)? { return Ok(None); }
                        let frame=Self::vm_frame(&self.program,&mut self.heap,self.module_env,"$$strictSet",vec![object,key,value],None)?;self.push_frame(frame);
                    }
                    Op::Branch(test, target) => {
                        let locals = self.frames.last().unwrap().locals()?;
                        let value = eval(&test, locals, &mut self.heap)?;
                        if !super::vm_eval::truthy(&value) {
                            self.frames.last_mut().unwrap().pc = target;
                        }
                    }
                    Op::Jump(target) => self.frames.last_mut().unwrap().pc = target,
                    Op::ForNext {
                        items,
                        index,
                        item,
                        end,
                    } => {
                        let frame = self.frames.last_mut().unwrap();
                        let locals = frame.vm_locals.as_mut().context("missing VM locals")?;
                        let VmValue::Reference(id) = locals.get(&items).context("missing array")? else { bail!("for...of requires an array") };
                        if matches!(self.heap.get(*id)?,HeapNode::Iterator(_)) {
                            let value=super::js_collections::next(&mut self.heap,*id)?;
                            if let Some(value)=value {locals.insert(item,value);}else{frame.pc=end;}
                            return Ok(None);
                        }
                        let HeapNode::Array(values) = self.heap.get(*id)? else { bail!("TypeError: for...of requires a supported iterable") };
                        let VmValue::Number(cursor) = locals.get(&index).context("invalid internal iterator cursor")? else { bail!("invalid internal iterator cursor") };
                        let cursor = cursor.get() as usize;
                        if let Some(value) = values.get(cursor).cloned() {
                            locals.insert(item, value);
                            locals.insert(index, VmValue::Number(JsNumber::new((cursor + 1) as f64)));
                        } else {
                            frame.pc = end;
                        }
                    }

                    Op::Catch { target, name } => {
                        let frame = self.frames.last_mut().unwrap();
                        frame.handlers.push((target, name));
                        frame.handler_envs.push(frame.env);
                    }
                    Op::EndCatch => {
                        let frame = self.frames.last_mut().unwrap();
                        frame.handlers.pop();
                        frame.handler_envs.pop();
                    }
                    Op::Throw(e) => {
                        let locals = self.frames.last().unwrap().locals()?;
                        let value = eval(&e, locals, &mut self.heap)?;
                        self.throw(value, &at);
                    }
                    Op::Append {
                        target,
                        items,
                        destination,
                    } => {
                        let locals = self.frames.last().unwrap().locals()?;
                        let values = items
                            .iter()
                            .map(|item| eval(item, locals, &mut self.heap))
                            .collect::<Result<Vec<_>>>()?;
                        let array = eval(&Expr::Variable(target), locals, &mut self.heap)?;
                        let VmValue::Reference(array) = array else { bail!("result collection requires an array") };
                        let length = self.heap.append(array, values)?;
                        self.frames.last_mut().unwrap().vm_locals.as_mut().unwrap().insert(destination, VmValue::Number(JsNumber::new(length as f64)));
                    }
                    Op::Await { value, destination } => {
                        let locals = self.frames.last().unwrap().locals()?;
                        let value = eval(&value, locals, &mut self.heap)?;
                        let id = super::promise::ContinuationId(self.next_continuation);
                        self.next_continuation = self.next_continuation.checked_add(1).context("continuation id exhausted")?;
                        let boundary = self.frames.iter().rposition(|frame| frame.completion.is_some() || frame.generator.is_some_and(|id|self.is_async_generator(id))).unwrap_or(0);
                        let frames = self.frames.split_off(boundary);
                        self.continuations.insert(id, SuspendedCoroutine { frames, destination, at: at.clone() });
                        if let VmValue::Reference(promise) = value {
                            if matches!(self.heap.get(promise)?, HeapNode::Promise { .. }) {
                                self.promises.subscribe_heap_promise(&mut self.heap, promise, id)?;
                            } else { self.promises.enqueue(id, super::promise::Outcome::Fulfilled(VmValue::Reference(promise))); }
                        } else { self.promises.enqueue(id, super::promise::Outcome::Fulfilled(value)); }
                    }
                    Op::GeneratorStart => { self.suspend_generator_start()?; }
                    Op::Yield { value, destination, return_target, delegated } => {
                        let value = eval(&value, self.frames.last().unwrap().locals()?, &mut self.heap)?;
                        self.suspend_generator_yield(value, destination, return_target, delegated)?;
                    }
                    Op::Return(e) => {
                        let locals = self.frames.last().unwrap().locals()?;
                        let mut value = eval(&e, locals, &mut self.heap)?;
                        let frame = self.frames.pop().unwrap();
                        if let Some(id) = frame.generator {
                            self.complete_generator(id);
                            if self.is_async_generator(id) {
                                let result=self.generator_result(value,true)?;
                                self.async_generator_complete_step(id,super::promise::Outcome::Fulfilled(result))?;
                                self.drain_async_generator(id)?;
                                return Ok(None);
                            }
                            let result = self.generator_result(value, true)?;
                            self.store(frame.destination, result)?;
                            return Ok(None);
                        }
                        if let Some(env)=frame.derived_env && !matches!(value,VmValue::Reference(_)) {
                            ensure!(value==VmValue::Undefined,"TypeError: Derived constructors may only return object or undefined");
                            value=self.heap.lookup_binding(env,"\0this")?;
                        }
                        if let Some(receiver)=frame.constructed && !matches!(value,VmValue::Reference(_)){value=receiver;}
                        let initializer = if let Some(env)=frame.super_env {
                            self.heap.initialize_this(env,value.clone())?;
                            let active=self.heap.lookup_binding(env,"\0activeConstructor")?;
                            if let VmValue::Reference(id)=active { self.heap.class_initializer(id).map(|init|(init,value.clone())) } else { None }
                        } else { None };
                        if let Some(promise) = frame.completion {
                            self.resolve_async(Some(promise), value)?;
                        } else if frame.task_callback {
                            // Ignore a callback's return value, including promises.
                        } else if let Some(parent) = self.frames.last_mut() {
                            if let Some(dest) = frame.destination {
                                parent.vm_locals.as_mut().unwrap().insert(dest, value);
                            }
                        } else {
                            self.resolve_async(None, value)?;
                        }
                        if let Some((init, receiver)) = initializer { self.invoke_receiver(init,receiver,vec![],None)?; }
                    }
                    Op::Effect(destination, effect) => {
                        let locals = self.frames.last().unwrap().locals()?;
                        let prepared: PreparedAction = match effect {
                            Effect::PromiseConstruct { executor } => {
                                let executor = eval(&executor, locals, &mut self.heap)?;
                                self.promise_constructor(executor,destination,&at)?;
                                return Ok(None);
                            }
                            Effect::PromiseCreate { reject, value } => {
                                let value = eval(&value, locals, &mut self.heap)?;
                                if !reject && let VmValue::Reference(id) = &value
                                    && matches!(self.heap.get(*id)?, HeapNode::Promise { .. })
                                {
                                    self.store(destination, value)?;
                                } else {
                                    let id = self.promises.create_heap_promise(&mut self.heap)?;
                                    self.async_promises.push(id);
                                    let outcome = if reject { super::promise::Outcome::Rejected(value) } else { super::promise::Outcome::Fulfilled(value) };
                                    self.promises.settle_heap_promise(&mut self.heap, id, outcome)?;
                                    self.store(destination, VmValue::Reference(id))?;
                                }
                                return Ok(None);
                            }
                            Effect::Call(name, args) => {
                                let args = vm_args(&args, locals, &mut self.heap)?;
                                let function = self
                                    .program
                                    .functions
                                    .get(&name)
                                    .with_context(|| format!("unknown function {name}"))?;
                                if function.virtual_task && !function.partial_virtual {
                                    // AI functions take exactly their declared parameters.
                                    ensure!(
                                        args.len() == function.params.len(),
                                        "{name}: expected {} arguments, got {}",
                                        function.params.len(),
                                        args.len()
                                    );
                                    // A top-level function closes over the module scope.
                                    let captured = self.captured(&name, self.module_env)?;
                                    let captured_scope = self.capture_virtual_metadata(self.module_env, &args, false);
                                    PreparedAction {
                                        kind: ActionKind::Call { name, args: json_args(args, &self.heap)?, captured },
                                        captured_scope,
                                    }
                                } else {
                                    // Functions see the module scope, not their caller's variables.
                                    ensure!(self.frames.len() < 64, "program call depth limit");
                                    let is_async = function.is_async;
                                    let mut frame = Self::vm_frame(
                                        &self.program,
                                        &mut self.heap,
                                        self.module_env,
                                        &name,
                                        args,
                                        destination,
                                    )?;
                                    if is_async {
                                        let promise = self.promises.create_heap_promise(&mut self.heap)?;
                                        self.async_promises.push(promise);
                                        self.store(frame.destination.take(), VmValue::Reference(promise))?;
                                        frame.completion = Some(promise);
                                        frame.origin = self.async_origin();
                                    }
                                    self.push_frame(frame);
                                    return Ok(None);
                                }
                            }
                            Effect::Apply { callee, args } => {
                                let callee = eval(&callee, locals, &mut self.heap)?;
                                let args = eval(&args, locals, &mut self.heap)?;
                                let VmValue::Reference(id) = args else { bail!("TypeError: apply arguments must be an array") };
                                let HeapNode::Array(args) = self.heap.get(id)? else { bail!("TypeError: apply arguments must be an array") };
                                let args = args.clone();
                                match self.invoke(callee, args, destination.clone())? {
                                    Some(kind) => kind,
                                    None => return Ok(None),
                                }
                            }
                            Effect::Invoke(call)=>{
                                let Invocation{callee,receiver,args,label}=*call;
                                let callee=eval(&callee,locals,&mut self.heap)?;
                                let receiver=eval(&receiver,locals,&mut self.heap)?;
                                let args=eval(&args,locals,&mut self.heap)?;
                                let args=super::js_functions::apply_args(&self.heap,args)?;
                                if !super::vm_eval::callable(&self.heap,&callee) {
                                    if let Some(label)=label {
                                        let label=eval(&label,locals,&mut self.heap)?;
                                        let name=super::js_value::to_string(&self.heap,&label)?;
                                        if callee == VmValue::Undefined
                                            && let VmValue::Reference(id) = receiver
                                            && let Some(val) = super::resource_handle::try_method(&mut self.heap, id, &name, &args)?
                                        {
                                            self.store(destination.clone(), val)?;
                                            return Ok(None);
                                        }
                                        if let Some(qualified) = crate::tools::tool_registry::ToolRegistry::qualified_hint(&name) {
                                            if receiver != VmValue::Undefined
                                                && let Some((lib, _)) = qualified.split_once('.')
                                            {
                                                bail!(
                                                    "TypeError: {name} is not a function (a local variable may have shadowed host library `{lib}`; call `require(\"{lib}\").{name}(...)` or rename that variable)"
                                                );
                                            }
                                            bail!("TypeError: {name} is not a function (did you mean `{qualified}(...)`?)");
                                        }
                                        bail!("TypeError: {} is not a function",name);
                                    }
                                }
                                match self.invoke_receiver(callee,receiver,args,destination.clone())? {Some(kind)=>kind,None=>return Ok(None)}
                            }
                            Effect::Construct(call)=>{
                                let callee=eval(&call.callee,locals,&mut self.heap)?;
                                let args=eval(&call.args,locals,&mut self.heap)?;
                                let args=super::js_functions::apply_args(&self.heap,args)?;
                                self.construct(callee,args,destination,&at)?;
                                return Ok(None);
                            }
                            Effect::Dynamic { callee, args } => {
                                let callee = eval(&callee, locals, &mut self.heap)?;
                                let args = vm_args(&args, locals, &mut self.heap)?;
                                match self.invoke(callee, args, destination.clone())? {
                                    Some(kind) => kind,
                                    None => return Ok(None),
                                }
                            }
                            Effect::Method { receiver, name, args } => {
                                let receiver = eval(&receiver, locals, &mut self.heap)?;
                                let args = vm_args(&args, locals, &mut self.heap)?;
                                match self.invoke_method(receiver, &name, args, destination.clone(), false)? {
                                    Some(kind) => kind,
                                    None => return Ok(None),
                                }
                            }
                            Effect::DynamicMethodApply { receiver, name, args } => {
                                let receiver = eval(&receiver, locals, &mut self.heap)?;
                                let name = eval(&name, locals, &mut self.heap)?;
                                let name = super::js_value::to_string(&self.heap, &name)?;
                                let args = eval(&args, locals, &mut self.heap)?;
                                let VmValue::Reference(id) = args else { bail!("TypeError: apply arguments must be an array") };
                                let HeapNode::Array(args) = self.heap.get(id)? else { bail!("TypeError: apply arguments must be an array") };
                                let args = args.clone();
                                match self.invoke_method(receiver, &name, args, destination.clone(), true)? {
                                    Some(kind) => kind,
                                    None => return Ok(None),
                                }
                            }
                            Effect::MethodApply { receiver, name, args, ignore_property } => {
                                let receiver = eval(&receiver, locals, &mut self.heap)?;
                                let args = eval(&args, locals, &mut self.heap)?;
                                let VmValue::Reference(id) = args else { bail!("TypeError: apply arguments must be an array") };
                                let HeapNode::Array(args) = self.heap.get(id)? else { bail!("TypeError: apply arguments must be an array") };
                                let args = args.clone();
                                match self.invoke_method(receiver, &name, args, destination.clone(), ignore_property)? {
                                    Some(kind) => kind,
                                    None => return Ok(None),
                                }
                            }
                            Effect::Require(library) => {
                                // A library this run already required is the same object.
                                if let Some(id) = self.required.get(&library) {
                                    self.store(destination, VmValue::Reference(*id))?;
                                    return Ok(None);
                                }
                                ActionKind::Require { library }.into()
                            }
                            Effect::RequireValue(value) => {
                                let value = eval(&value, locals, &mut self.heap)?;
                                let library = super::js_value::to_string(&self.heap, &value)?;
                                let library = library.strip_prefix("host:").unwrap_or(&library).to_owned();
                                if let Some(module) = self.builtin_module(&library)? {
                                    self.store(destination, module)?;
                                    return Ok(None);
                                }
                                if let Some(id) = self.required.get(&library) {
                                    self.store(destination, VmValue::Reference(*id))?;
                                    return Ok(None);
                                }
                                ActionKind::Require { library }.into()
                            }
                            Effect::Tool(name, args) => {
                                let values = vm_args(&args, locals, &mut self.heap)?;
                                let mut args = json_args(values, &self.heap)?;
                                let args = if args.is_empty() {
                                    json!({})
                                } else {
                                    args.remove(0)
                                };
                                ensure!(args.is_object(), "tool arguments must be an object");
                                ActionKind::Tool { name, args }.into()
                            }
                            Effect::ParallelReads(operations) => {
                                let mut resolved = Vec::with_capacity(operations.len());
                                for (name, args) in operations {
                                    let values = vm_args(&args, locals, &mut self.heap)?;
                                    let mut args = json_args(values, &self.heap)?;
                                    let args = if args.is_empty() {
                                        json!({})
                                    } else {
                                        args.remove(0)
                                    };
                                    ensure!(args.is_object(), "tool arguments must be an object");
                                    resolved.push((name, args));
                                }
                                ActionKind::ParallelReads {
                                    operations: resolved,
                                }
                                .into()
                            }
                            Effect::ParallelReadMap {
                                name,
                                items,
                                parameter,
                                args,
                            } => {
                                let list = eval(&items, locals, &mut self.heap)?;
                                let VmValue::Reference(list) = list else { bail!("Promise.all map needs an array") };
                                let HeapNode::Array(items) = self.heap.get(list)? else { bail!("Promise.all map needs an array") };
                                let items = items.clone();
                                ensure!(items.len() <= 32, "Promise.all supports at most 32 reads");
                                let mut resolved = Vec::with_capacity(items.len());
                                for item in &items {
                                    // The map parameter is bound for this item only.
                                    let scope = self.heap.new_environment(
                                        locals.env,
                                        [(parameter.clone(), item.clone())].into(),
                                        BTreeSet::new(),
                                    )?;
                                    let values = vm_args(&args, Locals { temps: locals.temps, env: Some(scope) }, &mut self.heap)?;
                                    let mut values = json_args(values, &self.heap)?;
                                    let args = if values.is_empty() {
                                        json!({})
                                    } else {
                                        values.remove(0)
                                    };
                                    ensure!(args.is_object(), "tool arguments must be an object");
                                    resolved.push((name.clone(), args));
                                }
                                ActionKind::ParallelReads {
                                    operations: resolved,
                                }
                                .into()
                            }
                            Effect::Fork(..) | Effect::Join(..) => bail!("saved fork/join programs are retired and cannot be resumed"),
                            Effect::Task {
                                name,
                                description,
                                input,
                            } => {
                                let value = eval(&input, locals, &mut self.heap)?;
                                let captured_scope =
                                    self.capture_virtual_metadata(locals.env, &[value.clone()], true);
                                PreparedAction {
                                    kind: ActionKind::Task {
                                        name,
                                        description,
                                        input: self.heap.to_json(&value)?,
                                    },
                                    captured_scope,
                                }
                            },
                        };
                        let mut kind = prepared.kind;
                        let asynchronous = match &mut kind {
                            ActionKind::Tool { name, .. } if name.starts_with("async-tool:") => { *name = name["async-tool:".len()..].to_owned(); true }
                            ActionKind::LibraryCall { library, .. } if library.starts_with("async:") => { *library = library["async:".len()..].to_owned(); true }
                            ActionKind::Call { name, .. } | ActionKind::Task { name, .. } => self.program.functions.get(name).is_some_and(|f| f.virtual_task && f.is_async),
                            _ => false,
                        };
                        let action = Action {
                            id: format!("program-{}", crate::server::agent_service::uuid_like()),
                            at: at.clone(),
                            destination: if asynchronous { None } else { destination.clone() },
                            kind,
                            captured_scope: prepared.captured_scope,
                        };
                        if asynchronous {
                            ensure!(self.async_actions.len() + self.inflight_async.len() + usize::from(self.active_async_action.is_some()) + self.active_async_batch.len() < 64, "too many pending async host actions");
                            let promise = self.promises.create_heap_promise(&mut self.heap)?;
                            self.async_promises.push(promise);
                            self.store(destination, VmValue::Reference(promise))?;
                            let scope = self.named_bindings();
                            self.async_actions.push_back(PendingAsyncAction { action, promise, scope });
                            return Ok(None);
                        }
                        return Ok(Some(action));
                    }
                }
                Ok(None)
            })();
            match outcome {
                Ok(Some(action)) => {
                    self.pending = Some(action.clone());
                    self.collect_dead_bindings();
                    return State::Action(action);
                }
                Ok(None) => {}
                Err(e) => {
                    if self.error.as_ref().is_some_and(is_resource_error) {
                        if let Some(error) = self.error.as_mut() { error["location"] = json!(work_site.as_ref().map(|(_,at)|at).unwrap_or(&at)); }
                        return State::Failed(self.error.clone().unwrap());
                    }
                    let error = self.runtime_error(format!("{e:#}"), &at);
                    self.throw(error, &at);
                }
            }
        }
        self.clear_scheduled_callbacks();
        self.error
            .as_ref()
            .map(|e| State::Failed(e.clone()))
            .unwrap_or_else(|| State::Returned(self.result.clone()))
    }

}
