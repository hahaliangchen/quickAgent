//! A generator is a heap identity plus durable suspended VM frames. Resuming
//! never replays its source or recreates parameter/closure environments.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Status {
    Initializing,
    Start,
    Executing,
    Yielded {
        destination: String,
        return_target: usize,
        delegated: bool,
    },
    Completed,
    AwaitReturn,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Generator {
    status: Status,
    frames: Vec<Frame>,
    #[serde(default)]
    asynchronous: bool,
    #[serde(default)]
    queue: VecDeque<AsyncRequest>,
}
impl Generator {
    pub(crate) fn initializing(asynchronous: bool) -> Self {
        Self {
            status: Status::Initializing,
            frames: Vec::new(),
            asynchronous,
            queue: VecDeque::new(),
        }
    }
    pub(super) fn pending(&self) -> bool {
        self.asynchronous && !self.queue.is_empty()
    }
    pub(crate) fn roots(&self) -> Vec<VmValue> {
        let mut roots = Vec::new();
        for request in &self.queue {
            roots.push(request.value.clone());
            roots.push(VmValue::Reference(request.promise));
        }
        for f in &self.frames {
            roots.extend(
                f.env
                    .into_iter()
                    .chain(f.handler_envs.iter().copied().flatten())
                    .chain(f.origin.iter().copied())
                    .chain(f.generator)
                    .chain(f.completion)
                    .chain(f.executor)
                    .chain(f.derived_env)
                    .chain(f.super_env)
                    .map(VmValue::Reference),
            );
            roots.extend(f.constructed.iter().cloned());
            roots.extend(f.generator_function.iter().cloned());
            if let Some(locals) = &f.vm_locals {
                roots.extend(locals.values().cloned());
            }
        }
        roots
    }
}

impl Runtime {
    pub(super) fn generator_result(&mut self, value: VmValue, done: bool) -> Result<VmValue> {
        Ok(VmValue::Reference(self.heap.allocate(HeapNode::Object(
            vec![
                ("value".into(), value),
                ("done".into(), VmValue::Boolean(done)),
            ],
        ))?))
    }

    pub(super) fn complete_generator(&mut self, id: HeapId) {
        if let Some(generator) = self.heap.generators.get_mut(&id) {
            generator.status = Status::Completed;
            generator.frames.clear();
        }
    }

    pub(super) fn suspend_generator_start(&mut self) -> Result<()> {
        let mut frame = self.frames.pop().context("generator frame missing")?;
        let callee = frame
            .generator_function
            .take()
            .context("generator start outside generator call")?;
        let asynchronous = self.program.functions[&frame.function].is_async;
        let id = self.heap.new_generator(&callee, asynchronous)?;
        frame.generator = Some(id);
        let destination = frame.destination.take();
        let generator = self
            .heap
            .generators
            .get_mut(&id)
            .context("generator slots missing")?;
        generator.status = Status::Start;
        generator.frames = vec![frame];
        if self.frames.is_empty() {
            self.root_result = Some(VmValue::Reference(id));
            Ok(())
        } else {
            self.store(destination, VmValue::Reference(id))
        }
    }

    pub(super) fn suspend_generator_yield(
        &mut self,
        value: VmValue,
        destination: String,
        return_target: usize,
        delegated: bool,
    ) -> Result<()> {
        let boundary = self
            .frames
            .iter()
            .rposition(|f| f.generator.is_some())
            .context("yield outside generator")?;
        let mut frames = self.frames.split_off(boundary);
        let id = frames[0].generator.unwrap();
        let caller = frames[0].destination.take();
        let generator = self
            .heap
            .generators
            .get_mut(&id)
            .context("generator slots missing")?;
        generator.status = Status::Yielded {
            destination,
            return_target,
            delegated,
        };
        generator.frames = frames;
        if generator.asynchronous {
            let result = self.generator_result(value, false)?;
            self.async_generator_complete_step(
                id,
                super::super::promise::Outcome::Fulfilled(result),
            )?;
            return self.drain_async_generator(id);
        }
        let result = if delegated {
            value
        } else {
            self.generator_result(value, false)?
        };
        self.store(caller, result)
    }

    pub(super) fn resume_generator(
        &mut self,
        receiver: VmValue,
        kind: &str,
        args: Vec<VmValue>,
        destination: Option<String>,
    ) -> Result<()> {
        let VmValue::Reference(id) = receiver else {
            bail!("TypeError: Generator method requires a generator receiver");
        };
        let status = self
            .heap
            .generators
            .get(&id)
            .context("TypeError: Generator method requires a generator receiver")?
            .status
            .clone();
        ensure!(
            !matches!(status, Status::Executing | Status::Initializing),
            "TypeError: Generator is already executing"
        );
        ensure!(
            !self.heap.generators[&id].asynchronous,
            "TypeError: Generator method requires a synchronous generator receiver"
        );
        let value = args.into_iter().next().unwrap_or(VmValue::Undefined);
        if matches!(status, Status::Completed) || matches!(status, Status::Start) && kind != "next"
        {
            self.complete_generator(id);
            if kind == "throw" {
                let at = self
                    .current_call_site()
                    .unwrap_or(Location { line: 0, column: 0 });
                self.throw(value, &at);
            } else {
                let result = self.generator_result(
                    if kind == "return" {
                        value
                    } else {
                        VmValue::Undefined
                    },
                    true,
                )?;
                self.store(destination, result)?;
            }
            return Ok(());
        }
        let generator = self.heap.generators.get_mut(&id).unwrap();
        generator.status = Status::Executing;
        let mut frames = std::mem::take(&mut generator.frames);
        ensure!(!frames.is_empty(), "generator continuation missing");
        frames[0].destination = destination;
        let mut injected_throw = None;
        if let Status::Yielded {
            destination,
            return_target,
            delegated,
        } = status
        {
            let resume = if delegated {
                VmValue::Reference(self.heap.allocate(HeapNode::Object(vec![
                    ("kind".into(), VmValue::String(kind.into())),
                    ("value".into(), value),
                ]))?)
            } else if kind == "throw" {
                injected_throw = Some(value);
                VmValue::Undefined
            } else {
                if kind == "return" {
                    frames.last_mut().unwrap().pc = return_target;
                }
                value
            };
            frames
                .last_mut()
                .unwrap()
                .vm_locals
                .as_mut()
                .unwrap()
                .insert(destination, resume);
        }
        let at = self
            .current_call_site()
            .unwrap_or(Location { line: 0, column: 0 });
        self.frames.extend(frames);
        if let Some(error) = injected_throw {
            self.throw(error, &at);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AsyncRequest {
    kind: String,
    value: VmValue,
    promise: HeapId,
    at: Location,
}

impl Runtime {
    pub(super) fn is_async_generator(&self, id: HeapId) -> bool {
        self.heap
            .generators
            .get(&id)
            .is_some_and(|g| g.asynchronous)
    }
    pub(super) fn record_async_generator_throw(
        &mut self,
        id: HeapId,
        value: &VmValue,
        at: &Location,
    ) {
        let site = json!({"location":at,"stack":self.stack()});
        let promise = self
            .heap
            .generators
            .get(&id)
            .and_then(|g| g.queue.front())
            .map(|r| r.promise);
        let thrown = if let VmValue::Reference(id) = value {
            Some(*id)
        } else {
            None
        };
        for key in promise.into_iter().chain(thrown) {
            self.throw_sites.entry(key).or_insert_with(|| site.clone());
        }
    }
    pub(super) fn async_generator_complete_step(
        &mut self,
        id: HeapId,
        outcome: super::super::promise::Outcome,
    ) -> Result<()> {
        let request = self
            .heap
            .generators
            .get_mut(&id)
            .context("async generator missing")?
            .queue
            .pop_front()
            .context("async generator request missing")?;
        self.promises
            .settle_heap_promise(&mut self.heap, request.promise, outcome)?;
        Ok(())
    }
    pub(super) fn enqueue_async_generator(
        &mut self,
        receiver: VmValue,
        kind: &str,
        args: Vec<VmValue>,
        destination: Option<String>,
    ) -> Result<()> {
        let promise = self.promises.create_heap_promise(&mut self.heap)?;
        self.async_promises.push(promise);
        self.store(destination, VmValue::Reference(promise))?;
        let id = match receiver {
            VmValue::Reference(id) if self.is_async_generator(id) => id,
            _ => {
                let at = self
                    .current_call_site()
                    .unwrap_or(Location { line: 0, column: 0 });
                let error = self.runtime_error(
                    "TypeError: AsyncGenerator method requires an async generator receiver".into(),
                    &at,
                );
                self.promises.settle_heap_promise(
                    &mut self.heap,
                    promise,
                    super::super::promise::Outcome::Rejected(error),
                )?;
                return Ok(());
            }
        };
        let at = self
            .current_call_site()
            .unwrap_or(Location { line: 0, column: 0 });
        self.heap
            .generators
            .get_mut(&id)
            .unwrap()
            .queue
            .push_back(AsyncRequest {
                kind: kind.into(),
                value: args.into_iter().next().unwrap_or(VmValue::Undefined),
                promise,
                at,
            });
        self.drain_async_generator(id)
    }
    pub(super) fn drain_async_generator(&mut self, id: HeapId) -> Result<()> {
        loop {
            let generator = self
                .heap
                .generators
                .get_mut(&id)
                .context("async generator missing")?;
            if matches!(
                generator.status,
                Status::Executing | Status::Initializing | Status::AwaitReturn
            ) {
                return Ok(());
            }
            let Some(request) = generator.queue.front().cloned() else {
                return Ok(());
            };
            let status = generator.status.clone();
            if matches!(status, Status::Completed)
                || matches!(status, Status::Start) && request.kind != "next"
            {
                self.complete_generator(id);
                if request.kind == "return" {
                    self.heap.generators.get_mut(&id).unwrap().status = Status::AwaitReturn;
                    let mut frame = Self::vm_frame(
                        &self.program,
                        &mut self.heap,
                        self.module_env,
                        "$$asyncGeneratorAwaitReturn",
                        vec![request.value],
                        None,
                    )?;
                    frame.generator = Some(id);
                    self.push_frame(frame);
                    return Ok(());
                }
                let outcome = if request.kind == "throw" {
                    super::super::promise::Outcome::Rejected(request.value)
                } else {
                    super::super::promise::Outcome::Fulfilled(
                        self.generator_result(VmValue::Undefined, true)?,
                    )
                };
                self.async_generator_complete_step(id, outcome)?;
                continue;
            }
            let generator = self.heap.generators.get_mut(&id).unwrap();
            generator.status = Status::Executing;
            let mut frames = std::mem::take(&mut generator.frames);
            ensure!(!frames.is_empty(), "async generator continuation missing");
            let mut thrown = None;
            if let Status::Yielded {
                destination,
                return_target,
                delegated,
            } = status
            {
                let value = if delegated {
                    VmValue::Reference(self.heap.allocate(HeapNode::Object(vec![
                        ("kind".into(), VmValue::String(request.kind.clone())),
                        ("value".into(), request.value),
                    ]))?)
                } else if request.kind == "throw" {
                    thrown = Some(request.value);
                    VmValue::Undefined
                } else {
                    if request.kind == "return" {
                        frames.last_mut().unwrap().pc = return_target;
                    }
                    request.value
                };
                frames
                    .last_mut()
                    .unwrap()
                    .vm_locals
                    .as_mut()
                    .unwrap()
                    .insert(destination, value);
            }
            self.frames.extend(frames);
            if let Some(error) = thrown {
                self.throw(error, &request.at);
            }
            return Ok(());
        }
    }
}
