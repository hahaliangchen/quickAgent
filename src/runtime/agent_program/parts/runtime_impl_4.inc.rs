impl Runtime {
    /// Dispatch virtual calls already issued by JS while preserving serial effects.
    pub fn next_concurrent(&mut self) -> ConcurrentState {
        let state = self.next();
        if let State::Action(action) = state {
            let parallel_frame = |action: &Action| matches!(action.kind, ActionKind::Call { .. } | ActionKind::Task { .. } | ActionKind::LibraryCall { .. });
            let mut actions = Vec::new();
            if self.active_async_action.as_ref().is_some_and(|pending| parallel_frame(&pending.action)) {
                self.pending = None;
                let pending = self.active_async_action.take().expect("active async action");
                actions.push(pending.action.clone());
                self.inflight_async.insert(pending.action.id.clone(), pending);
            }
            let mut retained = VecDeque::new();
            while let Some(pending) = self.async_actions.pop_front() {
                if parallel_frame(&pending.action) {
                    actions.push(pending.action.clone());
                    self.inflight_async.insert(pending.action.id.clone(), pending);
                } else { retained.push_back(pending); }
            }
            self.async_actions = retained;
            return if actions.is_empty() { ConcurrentState::Actions(vec![action]) } else { ConcurrentState::Actions(actions) };
        }
        match state {
            State::Waiting => ConcurrentState::Waiting,
            State::Returned(value) => ConcurrentState::Returned(value),
            State::Failed(error) => ConcurrentState::Failed(error),
            State::Action(_) => unreachable!(),
        }
    }
    #[cfg(test)]
    pub fn async_scope(&self, id: &str) -> Option<&BTreeMap<String, Value>> {
        self.inflight_async.get(id).map(|pending| &pending.scope)
    }
    pub fn inflight_actions(&self) -> Vec<Action> {
        self.inflight_async.values().map(|pending| pending.action.clone()).collect()
    }
    #[cfg(test)]
    pub(super) fn conformance(program:Program)->Result<Self> {
        let mut runtime=Self::new(program,super::ENTRY,vec![])?;
        super::conformance::install(&mut runtime.heap,runtime.module_env.context("missing test realm")?)?;
        Ok(runtime)
    }
    #[cfg(test)]
    pub(super) fn conformance_done(&self)->bool {
        self.module_env.and_then(|env|self.heap.lookup_binding(env,"__test262DoneCalled").ok())==Some(VmValue::Boolean(true))
    }
    fn refresh_instruction_segment(&mut self) {
        self.fuel = self.segment_limit;
        self.line_instructions.clear();
        self.segment_started_at = Some(std::time::Instant::now());
        self.loop_guard.clear();
    }


    /// Lightweight deterministic state fingerprint of the active frame + enclosing
    /// lexical environments + host queues, used by the React/Vue-style infinite loop
    /// guard to detect loops executing thousands of iterations with zero state change.
    fn loop_state_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();

        fn hash_val(val: &VmValue, heap: &super::heap::Heap, depth: u8, h: &mut std::collections::hash_map::DefaultHasher) {
            std::mem::discriminant(val).hash(h);
            match val {
                VmValue::Undefined | VmValue::Null => {}
                VmValue::Boolean(b) => b.hash(h),
                VmValue::Symbol(id) => id.hash(h),
                VmValue::Number(n) => n.get().to_bits().hash(h),
                VmValue::String(s) => {
                    s.len().hash(h);
                    let bytes = s.as_bytes();
                    if bytes.len() <= 64 {
                        bytes.hash(h);
                    } else {
                        bytes[..32].hash(h);
                        bytes[bytes.len() - 32..].hash(h);
                    }
                }
                VmValue::Utf16(u) => {
                    u.len().hash(h);
                    if let Some(first) = u.first() { first.hash(h); }
                    if let Some(last) = u.last() { last.hash(h); }
                }
                VmValue::Promise(pid) => format!("{pid:?}").hash(h),
                VmValue::Reference(id) => {
                    id.hash(h);
                    if depth == 0 {
                        return;
                    }
                    if let Ok(node) = heap.get(*id) {
                        std::mem::discriminant(node).hash(h);
                        match node {
                            HeapNode::Array(items) => {
                                items.len().hash(h);
                                if let Some(first) = items.first() {
                                    hash_val(first, heap, depth - 1, h);
                                }
                                if let Some(last) = items.last() {
                                    hash_val(last, heap, depth - 1, h);
                                }
                            }
                            HeapNode::Object(props) => {
                                props.len().hash(h);
                                for (k, v) in props.iter().take(16) {
                                    k.hash(h);
                                    hash_val(v, heap, depth - 1, h);
                                }
                            }
                            HeapNode::Iterator(it) => {
                                it.cursor.hash(h);
                                it.limit.hash(h);
                            }
                            HeapNode::Collection(col) => {
                                format!("{col:?}").len().hash(h);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        self.frames.len().hash(&mut h);
        self.log_bytes.hash(&mut h);
        self.async_actions.len().hash(&mut h);
        self.inflight_async.len().hash(&mut h);
        if let Some(frame) = self.frames.last() {
            if let Some(locals) = &frame.vm_locals {
                locals.len().hash(&mut h);
                for (k, v) in locals {
                    k.hash(&mut h);
                    hash_val(v, &self.heap, 1, &mut h);
                }
            }
            let mut env_opt = frame.env;
            let mut hops = 0u8;
            while let Some(env_id) = env_opt {
                if hops >= 4 {
                    break;
                }
                hops += 1;
                if let Ok(HeapNode::Environment { parent, bindings, .. }) = self.heap.get(env_id) {
                    bindings.len().hash(&mut h);
                    for (k, v) in bindings {
                        if k.starts_with('\0') {
                            continue;
                        }
                        k.hash(&mut h);
                        hash_val(v, &self.heap, 1, &mut h);
                    }
                    env_opt = *parent;
                } else {
                    break;
                }
            }
        }
        h.finish()
    }

    fn infinite_loop_error(&self, reason: &str, detail: String) -> Value {
        let mut hot: Vec<_> = self
            .line_instructions
            .iter()
            .flat_map(|(function, lines)| {
                lines.iter().map(move |(line, count)| {
                    json!({"function": function, "line": line, "instructions": count})
                })
            })
            .collect();
        hot.sort_by(|a, b| b["instructions"].as_u64().cmp(&a["instructions"].as_u64()));
        hot.truncate(8);
        let location = self.work_site().map(|(_, at)| json!(at));
        let elapsed_ms = self
            .segment_started_at
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        json!({
            "name": "InfiniteLoopError",
            "code": "INFINITE_LOOP_DETECTED",
            "reason": reason,
            "message": detail,
            "total_instructions": self.total_instructions,
            "segment_elapsed_ms": elapsed_ms,
            "location": location,
            "hot_lines": hot
        })
    }
    #[cfg(test)]
    pub(super) fn gc_collections(&self) -> usize { self.heap.gc_collections }
    /// Must run between instructions: temporaries live only in frames and roots.
    fn collect_garbage(&mut self) -> Result<()> {
        self.async_promises.retain(|id| match self.heap.get(*id) {
            Ok(HeapNode::Promise { outcome: None, .. }) => true,
            Ok(HeapNode::Promise { outcome: Some(super::promise::Outcome::Rejected(_)), handled: false, .. }) => true,
            _ => false,
        });
        let mut roots = self.promises.roots();
        self.callback_roots(&mut roots);
        roots.extend(self.heap.generators.iter().filter(|(_,g)|g.pending()).map(|(id,_)|VmValue::Reference(*id)));
        roots.extend(self.async_promises.iter().copied().map(VmValue::Reference));
        roots.extend(self.root_result.iter().cloned());
        roots.extend(
            self.active_segment_rollback
                .values()
                .flatten()
                .map(|(v, _)| v.clone()),
        );
        roots.extend(
            self.builtin_namespaces
                .values()
                .copied()
                .map(VmValue::Reference),
        );
        roots.extend(self.async_actions.iter().map(|action| VmValue::Reference(action.promise)));
        roots.extend(self.active_async_action.iter().map(|action| VmValue::Reference(action.promise)));
        roots.extend(self.active_reaction.iter().map(|action| VmValue::Reference(action.promise)));
        roots.extend(self.active_async_batch.iter().map(|action| VmValue::Reference(action.promise)));
        roots.extend(self.inflight_async.values().map(|action| VmValue::Reference(action.promise)));
        roots.extend(self.forwarding.values().chain(self.adopting.values()).flatten().copied().map(VmValue::Reference));
        for reaction in self.reactions.values() {
            roots.extend([reaction.fulfilled.clone(), reaction.rejected.clone(), VmValue::Reference(reaction.target)]);
            roots.extend(reaction.origin.iter().copied().map(VmValue::Reference));
        }
        for frame in self.frames.iter().chain(self.continuations.values().flat_map(|c| c.frames.iter())) {
            roots.extend(frame.generator.map(VmValue::Reference));
            roots.extend(frame.generator_function.iter().cloned());
            roots.extend(frame.constructed.iter().cloned());
            roots.extend(frame.derived_env.map(VmValue::Reference));
            roots.extend(frame.super_env.map(VmValue::Reference));
            if let Some(locals) = &frame.vm_locals { roots.extend(locals.values().cloned()); }
            roots.extend(frame.origin.iter().copied().map(VmValue::Reference));
            roots.extend(frame.executor.map(VmValue::Reference));
            roots.extend(
                frame
                    .env
                    .iter()
                    .chain(frame.handler_envs.iter().flatten())
                    .map(|id| VmValue::Reference(*id)),
            );
        }
        roots.extend(self.module_env.map(VmValue::Reference));
        roots.extend(self.required.values().copied().map(VmValue::Reference));
        self.heap.collect(&roots)?;
        let heap = &self.heap;
        self.throw_sites.retain(|id, _| heap.get(*id).is_ok());
        Ok(())
    }
    fn work_site(&self) -> Option<(String, Location)> {
        self.frames.iter().rev().find_map(|frame| {
            if frame.function.starts_with("$$") { frame.work_origin.clone() }
            else {
                self.program.functions.get(&frame.function)?.code
                    .get(frame.pc.saturating_sub(1)).map(|i|(frame.function.clone(), i.at.clone()))
            }
        })
    }
    fn push_frame(&mut self, mut frame: Frame) {
        if frame.function.starts_with("$$") && frame.work_origin.is_none() {
            frame.work_origin = self.work_site();
        }
        self.frames.push(frame);
    }
    fn budget_error(&self, timed_out: bool) -> Value {
        let mut hot:Vec<_>=self.line_instructions.iter().flat_map(|(function,lines)|
            lines.iter().map(move |(line,count)|json!({"function":function,"line":line,"instructions":count}))).collect();
        hot.sort_by(|a,b|b["instructions"].as_u64().cmp(&a["instructions"].as_u64()));
        hot.truncate(8);
        let location=self.work_site().map(|(_,at)|json!(at));
        let elapsed=chrono::Utc::now().timestamp_millis().saturating_sub(self.started_at_ms).max(0);
        json!({"name":"ResourceLimitError","code":if timed_out {"PROGRAM_TIME_BUDGET_EXHAUSTED"} else {"INSTRUCTION_BUDGET_EXHAUSTED"},
            "message":if timed_out {format!("Program time budget exhausted: elapsed {elapsed} ms, limit {} ms. State and completed effects are retained; revise the remaining work, do not replay completed operations.",self.time_limit_ms)}
                else {format!("Instruction budget exhausted between host interactions: used {} of {} instructions (total {}). Read hot_lines and split or optimize the computation; completed effects must not be replayed.",self.segment_limit.saturating_sub(self.fuel),self.segment_limit,self.total_instructions)},
            "limit":self.segment_limit,"used":self.segment_limit.saturating_sub(self.fuel),"total_instructions":self.total_instructions,
            "total_vm_steps":self.total_vm_steps,"meter":"user-work-v1",
            "elapsed_ms":elapsed,"limit_ms":self.time_limit_ms,"location":location,"hot_lines":hot})
    }
    pub fn remaining_time(&self) -> std::time::Duration {
        #[cfg(test)]
        if self.test_deadline_after_steps.is_some_and(|steps| self.total_vm_steps >= steps) {
            return std::time::Duration::ZERO;
        }
        let elapsed=chrono::Utc::now().timestamp_millis().saturating_sub(self.started_at_ms).max(0);
        std::time::Duration::from_millis(self.time_limit_ms.saturating_sub(elapsed).max(0) as u64)
    }
    #[cfg(test)]
    pub(crate) fn set_test_deadline_after_steps(&mut self, steps: u64) {
        self.test_deadline_after_steps = Some(steps);
    }
    pub fn expire_time_budget(&mut self) { self.fail_capacity(self.budget_error(true)); }
    /// Import host state into the serialized VM heap, shared by REPL segments.
    pub fn set_global_json(&mut self, name: &str, value: Value) -> Result<()> {
        self.heap.initialize_intrinsics()?;
        let Some(VmValue::Reference(global)) = self.heap.intrinsic("globalThis") else {
            anyhow::bail!("VM global object unavailable");
        };
        let value = self.heap.from_json(&value)?;
        self.heap.set_property(global, name.into(), value)
    }

    pub(crate) fn remember_failed_edit(&mut self, draft: Value) -> Result<()> {
        self.set_global_json("__lastFailedEdit", draft)?;
        let Some(VmValue::Reference(global)) = self.heap.intrinsic("globalThis") else {
            anyhow::bail!("VM global object unavailable");
        };
        let newest = self.heap.property(global, "__lastFailedEdit")?;
        let prior = match self.heap.property(global, "__failedEdits")? {
            VmValue::Reference(id) => match self.heap.get(id)? {
                HeapNode::Array(items) => &items[items.len().saturating_sub(2)..],
                _ => &[],
            },
            _ => &[],
        };
        let ring = self.heap.allocate(HeapNode::Array(prior.iter().cloned().chain([newest]).collect()))?;
        self.heap.set_property(global, "__failedEdits".into(), VmValue::Reference(ring))
    }

    pub fn resume_action(&mut self, id: &str, value: Value, failed: bool) -> Result<()> {
        if failed && stops_program(&value) {
            let at = self.inflight_async.get(id).map(|p| p.action.at.clone())
                .or_else(|| self.pending.as_ref().filter(|p| p.id == id).map(|p| p.at.clone()))
                .context("unknown or already completed action")?;
            self.inflight_async.remove(id);
            let mut error = value;
            error["location"] = json!(at);
            self.fail_capacity(error);
            return Ok(());
        }
        if let Some(pending) = self.inflight_async.get(id).cloned() {
            self.refresh_instruction_segment();
            let outcome = if failed { super::promise::Outcome::Rejected(self.host_error(value, &pending.action.at)) }
                else { super::promise::Outcome::Fulfilled(self.heap.from_json(&value)?) };
            self.promises.settle_heap_promise(&mut self.heap, pending.promise, outcome)?;
            self.inflight_async.remove(id);
            return Ok(());
        }
        ensure!(self.pending.as_ref().is_some_and(|action| action.id == id), "unknown or already completed action");
        self.resume(value, failed)
    }
    pub fn resume(&mut self, value: Value, failed: bool) -> Result<()> {
        ensure!(
            self.program.version == VERSION,
            "program language version changed; original source and results remain saved"
        );
        ensure!(self.program.prelude_hash == super::prelude::hash(), "program prelude changed");
        self.migrate_frames()?;
        if failed && stops_program(&value) {
            let mut error = value;
            error["location"] = json!(self.pending.as_ref().context("no suspended effect")?.at);
            self.fail_capacity(error);
            return Ok(());
        }
        ensure!(self.pending.is_some(),"no suspended effect");
        self.refresh_instruction_segment();
        if !self.active_async_batch.is_empty() {
            let action = self.pending.as_ref().context("no suspended batch")?.clone();
            let results = if failed {
                vec![(true, value); self.active_async_batch.len()]
            } else {
                let items = value.as_array().context("async read batch must return per-item results")?;
                ensure!(items.len() == self.active_async_batch.len(), "async read batch result count mismatch");
                items.iter().map(|item| {
                    let failed = item.get("failed").and_then(Value::as_bool).context("async read item missing failed flag")?;
                    let value = item.get("value").cloned().context("async read item missing value")?;
                    Ok((failed, value))
                }).collect::<Result<Vec<_>>>()?
            };
            let mut outcomes = Vec::with_capacity(results.len());
            for (failed, value) in results {
                outcomes.push(if failed { super::promise::Outcome::Rejected(self.host_error(value, &action.at)) }
                    else { super::promise::Outcome::Fulfilled(self.heap.from_json(&value)?) });
            }
            self.pending = None;
            for (item, outcome) in std::mem::take(&mut self.active_async_batch).into_iter().zip(outcomes) {
                self.promises.settle_heap_promise(&mut self.heap, item.promise, outcome)?;
            }
            return Ok(());
        }
        // A required library becomes an object of its methods, kept for this run.
        let linked = match self.pending.as_ref().map(|action| &action.kind) {
            Some(ActionKind::Require { library }) if !failed => Some(self.link(&library.clone(), &value)?),
            _ => None,
        };
        let imported = if !failed && ((self.active_async_action.is_some() || self.active_reaction.is_some()) || self.pending.as_ref().and_then(|action| action.destination.as_ref()).is_some()) {
            Some(match linked { Some(module) => module, None => self.heap.from_json(&value)? })
        } else { None };
        let action = self.pending.take().context("no suspended effect")?;
        if let Some(pending) = self.active_async_action.take().or_else(|| self.active_reaction.take()) {
            ensure!(pending.action.id == action.id, "async effect id mismatch");
            let outcome = if failed {
                super::promise::Outcome::Rejected(self.host_error(value, &action.at))
            } else {
                super::promise::Outcome::Fulfilled(imported.context("missing imported async result")?)
            };
            self.promises.settle_heap_promise(&mut self.heap, pending.promise, outcome)?;
            return Ok(());
        }
        if failed {
            self.raise(value, &action.at);
        } else if let Some(name) = action.destination {
            let value = imported.context("missing imported effect result")?;
            self.frames.last_mut().context("effect has no caller frame")?
                .vm_locals.as_mut().context("missing VM locals")?.insert(name, value);
        }
        Ok(())
    }
    pub fn next(&mut self) -> State {
        let state = self.run_until_suspended();
        if matches!(state, State::Returned(_) | State::Failed(_)) {
            if matches!(state, State::Failed(_)) {
                if let Some(environment) = self.module_env {
                    for (name, previous) in std::mem::take(&mut self.active_segment_rollback) {
                        let _ = self.heap.rollback_segment_binding(environment, &name, previous);
                    }
                } else {
                    self.active_segment_rollback.clear();
                }
            } else {
                self.active_segment_rollback.clear();
            }
        }
        // The host snapshots a suspended program, so leave no garbage in it.
        // Actions hold JSON only; everything they resume into is rooted.
        if matches!(state, State::Action(_) | State::Waiting) && self.heap.has_uncollected()
            && let Err(error) = self.collect_garbage() {
            return self.collection_failed(error);
        }
        state
    }

}
