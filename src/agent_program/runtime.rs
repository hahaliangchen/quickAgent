use super::*;
use anyhow::{Context, Result, bail, ensure};
use super::heap::{HeapId, HeapNode, VmValue, JsNumber};
use super::vm_eval::{Locals, eval, is_temp};
use std::collections::{BTreeSet, VecDeque};
#[path = "runtime_timers.rs"]
mod timers;
#[path = "runtime_generators.rs"]
mod generators;
pub(super) use generators::Generator;

/// Capture is program memory, independent of any AI's context budget.
pub const CONSOLE_CAPTURE_LIMIT_BYTES: usize = 16 * 1024 * 1024;
// The recorded inventory/audit segments peak below 270k work units after
// native mutation fast paths. Leave >7x headroom, retaining bounded loops.
pub const INSTRUCTION_LIMIT: usize = 2_000_000;
pub const PROGRAM_TIME_LIMIT_MS: i64 = 30 * 60 * 1000;
fn instruction_limit() -> usize { INSTRUCTION_LIMIT }
fn program_time_limit() -> i64 { PROGRAM_TIME_LIMIT_MS }
pub fn is_resource_error(error: &Value) -> bool {
    matches!(error["code"].as_str(), Some("OUTPUT_OVER_CAPACITY" | "SHELL_OUTPUT_OVER_CAPACITY" | "INSTRUCTION_BUDGET_EXHAUSTED" | "PROGRAM_TIME_BUDGET_EXHAUSTED"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    function: String,
    pc: usize,
    locals: BTreeMap<String, Value>,
    /// Compiler temporaries. Source variables live in `env`.
    #[serde(default)]
    vm_locals: Option<BTreeMap<String, VmValue>>,
    /// The frame's current lexical environment.
    #[serde(default)]
    env: Option<HeapId>,
    destination: Option<String>,
    handlers: Vec<(usize, String)>,
    /// The environment each handler restores, parallel to `handlers`.
    #[serde(default)]
    handler_envs: Vec<Option<HeapId>>,
    #[serde(default)]
    completion: Option<HeapId>,
    #[serde(default)]
    executor: Option<HeapId>,
    #[serde(default)]
    origin: Vec<HeapId>,
    /// A host callback is a fresh task, never a new root return value.
    #[serde(default)]
    task_callback: bool,
    #[serde(default)]
    constructed: Option<VmValue>,
    #[serde(default)]
    diagnostic_at: Option<Location>,
    /// User call site owning work done by a resumable built-in. Persist it
    /// with the frame so suspension cannot lose attribution.
    #[serde(default)]
    work_origin: Option<(String, Location)>,
    #[serde(default)]
    derived_env: Option<HeapId>,
    #[serde(default)]
    super_env: Option<HeapId>,
    /// Root of a generator invocation, including call-time initialization.
    #[serde(default)]
    generator: Option<HeapId>,
    /// Instance creation follows parameter evaluation, which may itself suspend.
    #[serde(default)]
    generator_function: Option<VmValue>,
}
impl Frame {
    fn locals(&self) -> Result<Locals<'_>> {
        Ok(Locals {
            temps: self.vm_locals.as_ref().context("missing VM locals")?,
            env: self.env,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SuspendedCoroutine {
    frames: Vec<Frame>,
    destination: String,
    at: Location,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PromiseReaction {
    #[serde(default)]
    at: Option<Location>,
    #[serde(default)]
    work_origin: Option<(String, Location)>,
    fulfilled: VmValue,
    rejected: VmValue,
    target: HeapId,
    origin: Vec<HeapId>,
    #[serde(default)]
    scope: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingAsyncAction {
    action: Action,
    promise: HeapId,
    scope: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Runtime {
    pub program: Program,
    // Retained for old snapshots; virtual calls always use scoped dispatch.
    #[serde(default)]
    pub scoped_calls: bool,
    frames: Vec<Frame>,
    #[serde(default)]
    inherited_scope: BTreeMap<String, Value>,
    // Persist Promise state alongside the serial effect and asynchronous queue.
    #[serde(default)]
    promises: super::promise::PromiseScheduler,
    #[serde(default)]
    continuations: BTreeMap<super::promise::ContinuationId, SuspendedCoroutine>,
    #[serde(default)]
    next_continuation: u64,
    #[serde(default)]
    timers: BTreeMap<u64, timers::Timer>,
    #[serde(default)]
    next_timer_id: u64,
    #[serde(default)]
    microtasks: BTreeMap<super::promise::ContinuationId, timers::Callback>,
    #[cfg(test)]
    #[serde(skip)]
    timer_test_time: Option<i64>,
    #[serde(default)]
    forwarding: BTreeMap<super::promise::ContinuationId, Option<HeapId>>,
    /// Queued adoptions of a Promise, as JavaScript's
    /// NewPromiseResolveThenableJob; the job's value is the adopted Promise.
    #[serde(default)]
    adopting: BTreeMap<super::promise::ContinuationId, Option<HeapId>>,
    #[serde(default)]
    reactions:BTreeMap<super::promise::ContinuationId, PromiseReaction>,
    #[serde(default)]
    async_promises: Vec<HeapId>,
    #[serde(default)]
    root_result: Option<VmValue>,
    /// Where a value was thrown out of an async function, keyed by the
    /// rejected Promise and by the thrown object, for unhandled reports.
    #[serde(default)]
    throw_sites: BTreeMap<HeapId, Value>,
    #[serde(default)]
    heap: super::heap::Heap,
    /// Top-level declarations. Inherited caller names form its parent scope.
    #[serde(default)]
    module_env: Option<HeapId>,
    pub pending: Option<Action>,
    #[serde(default)]
    async_actions: VecDeque<PendingAsyncAction>,
    #[serde(default)]
    active_async_action: Option<PendingAsyncAction>,
    #[serde(default)]
    active_reaction: Option<PendingAsyncAction>,
    #[serde(default)]
    active_async_batch: Vec<PendingAsyncAction>,
    #[serde(default)]
    inflight_async: BTreeMap<String, PendingAsyncAction>,
    /// What the program printed with console, for the AI that wrote it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    logs: Vec<String>,
    #[serde(default)]
    log_bytes: usize,
    /// The rejected record is retained for the host to archive, never delivered
    /// as a successful prefix. Old snapshots have no such record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rejected_console: Option<String>,
    /// Libraries this run has required, as in Node's module cache: requiring
    /// one again gives the same object.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    required: BTreeMap<String, HeapId>,
    pub finished: bool,
    pub result: Value,
    pub error: Option<Value>,
    fuel: usize,
    #[serde(default = "instruction_limit")]
    segment_limit: usize,
    #[serde(default)]
    total_instructions: u64,
    /// Physical interpreter steps drive maintenance independently of billing.
    #[serde(default)]
    total_vm_steps: u64,
    #[cfg(test)]
    #[serde(skip)]
    test_deadline_after_steps: Option<u64>,
    #[serde(default)]
    line_instructions: BTreeMap<String, BTreeMap<usize,u64>>,
    #[serde(default)]
    started_at_ms: i64,
    #[serde(default = "program_time_limit")]
    time_limit_ms: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub at: Location,
    pub destination: Option<String>,
    pub kind: ActionKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ActionKind {
    Call {
        name: String,
        args: Vec<Value>,
        /// The `@uses` variables of a virtual function, read at the call.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        captured: BTreeMap<String, Value>,
    },
    Tool {
        name: String,
        args: Value,
    },
    /// Link a third-party library: the host answers with its methods'
    /// tool names, or fails, and the program goes on from this line.
    Require {
        library: String,
    },
    ParallelReads {
        operations: Vec<(String, Value)>,
    },
    Fork {
        entry: String,
        args: Vec<Value>,
    },
    Join {
        ids: Vec<String>,
    },
    Task {
        name: String,
        description: String,
        input: Value,
    },
}
#[derive(Clone, Debug)]
pub enum State {
    Action(Action),
    Waiting,
    Returned(Value),
    Failed(Value),
}

#[derive(Clone, Debug)]
pub enum ConcurrentState {
    Actions(Vec<Action>),
    Waiting,
    Returned(Value),
    Failed(Value),
}

fn vm_args(args: &[Expr], locals: Locals<'_>, heap: &mut super::heap::Heap) -> Result<Vec<VmValue>> {
    args.iter().map(|expr| eval(expr, locals, heap)).collect()
}

/// Positional arguments cross as a JSON array: an undefined argument is null.
fn json_args(args: Vec<VmValue>, heap: &super::heap::Heap) -> Result<Vec<Value>> {
    args.iter()
        .map(|value| match value {
            VmValue::Undefined => Ok(Value::Null),
            value => heap.to_json(value),
        })
        .collect()
}

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
        bindings
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
        let prefix = if library.ends_with("/promises") { "tool:async-tool:" } else { "tool:" };
        let mut properties = Vec::with_capacity(methods.len());
        for (method, tool) in methods {
            let tool = tool.as_str().context("a library method names its tool")?;
            let native = self.heap.native(&format!("{prefix}{tool}"))?;
            properties.push((method.clone(), VmValue::Reference(native)));
        }
        let id = self.heap.allocate(HeapNode::Object(properties))?;
        self.required.insert(library.to_owned(), id);
        Ok(VmValue::Reference(id))
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

    fn invoke_method(&mut self, receiver: VmValue, name: &str, args: Vec<VmValue>, destination: Option<String>, ignore_property: bool) -> Result<Option<ActionKind>> {
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

    /// Preserve complete records. Exhausting capture memory ends this program;
    /// catching an ordinary JS exception cannot turn missing evidence into success.
    fn print(&mut self, method: &str, args: &[VmValue]) -> Result<()> {
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
        let mut line = parts.join(" ");
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
    pub fn logs(&self) -> &[String] {
        &self.logs
    }
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
    ) -> Result<Option<ActionKind>> {
        self.invoke_receiver(callee,VmValue::Undefined,args,destination)
    }
    fn get_property(&mut self,target:VmValue,key:VmValue,receiver:VmValue,destination:Option<String>)->Result<Option<ActionKind>> {
        ensure!(!matches!(target,VmValue::Null|VmValue::Undefined),"TypeError: Cannot read properties of null or undefined");
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
    fn invoke_receiver(&mut self, callee:VmValue, receiver:VmValue, args:Vec<VmValue>, destination:Option<String>) -> Result<Option<ActionKind>> {self.invoke_receiver_mode(callee,receiver,args,destination,false)}
    fn invoke_receiver_mode(&mut self, mut callee:VmValue, mut receiver:VmValue, mut args:Vec<VmValue>, destination:Option<String>, constructing:bool) -> Result<Option<ActionKind>> {
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
                if f.virtual_task {
                    ensure!(
                        args.len() == f.params.len(),
                        "{function}: expected {} arguments, got {}",
                        f.params.len(),
                        args.len()
                    );
                    let captured = self.captured(&function, Some(environment))?;
                    return Ok(Some(ActionKind::Call {
                        name: function,
                        args: json_args(args, &self.heap)?,
                        captured,
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
                self.call_resolver(capability, reject, args.into_iter().next().unwrap_or(VmValue::Undefined))?;
                self.store(destination, VmValue::Undefined)?;
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
                        let helper=match method {"sort"=>Some("$$sort"),"toSorted"=>Some("$$toSorted"),"push"=>Some("$$arrayPush"),"pop"=>Some("$$arrayPop"),"fill"=>Some("$$arrayFill"),"reverse"=>Some("$$arrayReverse"),"shift"=>Some("$$arrayShift"),"unshift"=>Some("$$arrayUnshift"),"splice"=>Some("$$arraySplice"),"copyWithin"=>Some("$$arrayCopyWithin"),_=>None};
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
                    if let VmValue::Reference(id)=&receiver {
                        let valid=match self.heap.get(*id)? {
                            HeapNode::Collection(c)=>c.kind.name()==class,
                            HeapNode::Iterator(it)=>it.tag==class,
                            _=>false,
                        };
                        ensure!(valid,"TypeError: incompatible {class} method receiver");
                    }else{bail!("TypeError: incompatible {class} method receiver");}
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
                if let Some(tool) = name.strip_prefix("tool:") {
                    let mut args = json_args(args, &self.heap)?;
                    let args = if args.is_empty() { json!({}) } else { args.remove(0) };
                    ensure!(args.is_object(), "tool arguments must be an object");
                    return Ok(Some(ActionKind::Tool {
                        name: tool.to_owned(),
                        args,
                    }));
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
    /// Dispatch virtual calls already issued by JS while preserving serial effects.
    pub fn next_concurrent(&mut self) -> ConcurrentState {
        let state = self.next();
        if let State::Action(action) = state {
            let mut actions = Vec::new();
            if self.active_async_action.as_ref().is_some_and(|pending| matches!(pending.action.kind, ActionKind::Call { .. })) {
                self.pending = None;
                let pending = self.active_async_action.take().expect("active async action");
                actions.push(pending.action.clone());
                self.inflight_async.insert(pending.action.id.clone(), pending);
            }
            let mut retained = VecDeque::new();
            while let Some(pending) = self.async_actions.pop_front() {
                if matches!(pending.action.kind, ActionKind::Call { .. }) {
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
        self.fuel=self.segment_limit;
        self.line_instructions.clear();
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
    pub fn resume_action(&mut self, id: &str, value: Value, failed: bool) -> Result<()> {
        if failed && is_resource_error(&value) {
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
    pub fn is_pending_async_batch(&self) -> bool { !self.active_async_batch.is_empty() }
    pub fn resume(&mut self, value: Value, failed: bool) -> Result<()> {
        ensure!(
            self.program.version == VERSION,
            "program language version changed; original source and results remain saved"
        );
        ensure!(self.program.prelude_hash == super::prelude::hash(), "program prelude changed");
        self.migrate_frames()?;
        if failed && is_resource_error(&value) {
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
        // The host snapshots a suspended program, so leave no garbage in it.
        // Actions hold JSON only; everything they resume into is rooted.
        if matches!(state, State::Action(_) | State::Waiting) && self.heap.has_uncollected()
            && let Err(error) = self.collect_garbage() {
            return self.collection_failed(error);
        }
        state
    }
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
        if !self.finished && self.remaining_time().is_zero() {self.expire_time_budget();}
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
                        if let Ok(HeapNode::Native(name)) = &node && let Some(tool) = name.strip_prefix("tool:") {
                            let input = json_args(vec![value.clone()], &self.heap).and_then(|args| {
                                let input = args.into_iter().next().unwrap_or_else(|| json!({}));
                                ensure!(input.is_object(), "tool arguments must be an object");
                                Ok(input)
                            });
                            match input {
                                Ok(input) => {
                                    let asynchronous = tool.starts_with("async-tool:");
                                    let tool = tool.strip_prefix("async-tool:").unwrap_or(tool);
                                    let action = Action { id: format!("program-{}", crate::agent_service::uuid_like()), at: at.clone(), destination: None, kind: ActionKind::Tool { name: tool.into(), args: input } };
                                    let pending = PendingAsyncAction { action: action.clone(), promise: reaction.target, scope: reaction.scope };
                                    if asynchronous { self.async_actions.push_back(pending); continue; }
                                    self.pending = Some(action.clone());
                                    self.active_reaction = Some(pending);
                                    return State::Action(action);
                                }
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
                    let read = |action: &Action| matches!(&action.kind, ActionKind::Tool { name, .. } if matches!(name.as_str(), "read_file" | "read_file_lines" | "search_text" | "list_dir"));
                    if read(&pending.action) && self.async_actions.front().is_some_and(|item| read(&item.action)) {
                        let at = pending.action.at.clone();
                        let mut batch = vec![pending];
                        while batch.len() < 32 && self.async_actions.front().is_some_and(|item| read(&item.action)) {
                            batch.push(self.async_actions.pop_front().unwrap());
                        }
                        let operations = batch.iter().map(|item| match &item.action.kind {
                            ActionKind::Tool { name, args } => (name.clone(), args.clone()), _ => unreachable!(),
                        }).collect();
                        let action = Action { id: format!("program-{}", crate::agent_service::uuid_like()), at, destination: None, kind: ActionKind::ParallelReads { operations } };
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
            if self.fuel == 0 {
                self.fail_capacity(self.budget_error(false));
                return State::Failed(self.error.clone().unwrap());
            }
            self.total_vm_steps = self.total_vm_steps.saturating_add(1);
            if self.total_vm_steps % 256 == 0 {
                if self.remaining_time().is_zero() {
                    self.expire_time_budget();
                    return State::Failed(self.error.clone().unwrap());
                }
                if self.heap.collection_due() && let Err(error) = self.collect_garbage() {
                    return self.collection_failed(error);
                }
            }
            let (instruction, charge) = {
                let frame = self.frames.last_mut().unwrap();
                let instruction = self.program.functions[&frame.function].code[frame.pc].clone();
                // Prelude implementation steps are not user instructions.
                // One unit per entry and backward edge prices iterations and
                // still bounds a built-in loop independently of wall time.
                let charge = !frame.function.starts_with("$$") || frame.pc == 0 || match &instruction.op {
                    Op::Jump(target) | Op::Branch(_, target) => *target <= frame.pc,
                    _ => false,
                };
                frame.pc += 1;
                (instruction, charge)
            };
            let at = instruction.at.clone();
            let work_site = self.work_site();
            if charge {
                self.fuel -= 1;
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
                        let kind = match effect {
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
                                if function.virtual_task {
                                    // AI functions take exactly their declared parameters.
                                    ensure!(
                                        args.len() == function.params.len(),
                                        "{name}: expected {} arguments, got {}",
                                        function.params.len(),
                                        args.len()
                                    );
                                    // A top-level function closes over the module scope.
                                    let captured = self.captured(&name, self.module_env)?;
                                    ActionKind::Call { name, args: json_args(args, &self.heap)?, captured }
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
                                    if let Some(label)=label {let label=eval(&label,locals,&mut self.heap)?;bail!("TypeError: {} is not a function",super::js_value::to_string(&self.heap,&label)?);}
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
                                ActionKind::Require { library }
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
                                ActionKind::Tool { name, args }
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
                            }
                            Effect::Fork(..) | Effect::Join(..) => bail!("saved fork/join programs are retired and cannot be resumed"),
                            Effect::Task {
                                name,
                                description,
                                input,
                            } => {
                                let value = eval(&input, locals, &mut self.heap)?;
                                ActionKind::Task { name, description, input: self.heap.to_json(&value)? }
                            },
                        };
                        let mut kind = kind;
                        let asynchronous = match &mut kind {
                            ActionKind::Tool { name, .. } if name.starts_with("async-tool:") => { *name = name["async-tool:".len()..].to_owned(); true }
                            ActionKind::Call { name, .. } => self.program.functions.get(name).is_some_and(|f| f.virtual_task && f.is_async),
                            _ => false,
                        };
                        let action = Action {
                            id: format!("program-{}", crate::agent_service::uuid_like()),
                            at: at.clone(),
                            destination: if asynchronous { None } else { destination.clone() },
                            kind,
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
