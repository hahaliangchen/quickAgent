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
/// Default wall-clock safety cap (30s) per uninterrupted JS compute burst in
/// unbounded REPL mode (catches diverging infinite loops like `while(true){i++}`
/// while allowing legitimate heavy loops to run freely without arbitrary instruction caps).
pub const DEFAULT_SEGMENT_WALL_LIMIT_MS: u64 = 30_000;
fn instruction_limit() -> usize { INSTRUCTION_LIMIT }
fn program_time_limit() -> i64 { PROGRAM_TIME_LIMIT_MS }
/// A failure only the user can resolve (see single_agent/blockers.rs). Like a
/// resource error, program code cannot catch it: the program stops there.
pub const NEEDS_USER: &str = "NEEDS_USER";
pub fn is_user_blocker(error: &Value) -> bool {
    error["code"] == NEEDS_USER
}
fn stops_program(error: &Value) -> bool {
    is_resource_error(error) || is_user_blocker(error)
}
pub fn is_resource_error(error: &Value) -> bool {
    matches!(
        error["code"].as_str(),
        Some(
            "OUTPUT_OVER_CAPACITY"
                | "SHELL_OUTPUT_OVER_CAPACITY"
                | "INSTRUCTION_BUDGET_EXHAUSTED"
                | "PROGRAM_TIME_BUDGET_EXHAUSTED"
                | "INFINITE_LOOP_DETECTED"
        )
    )
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
    /// Pre-segment state for lexical names declared in the currently executing
    /// append-only segment, so TDZ rollback restores earlier values instead of
    /// deleting them.
    #[serde(default)]
    active_segment_rollback: BTreeMap<String, Option<(VmValue, bool)>>,
    /// Pre-injected static library namespaces (excluded from user variable overlays
    /// unless shadowed by user code).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    builtin_namespaces: BTreeMap<String, HeapId>,
    /// When true (Context-as-Code v2 REPL), arbitrary instruction caps are lifted
    /// so legitimate finite loops run freely; safety is enforced by React/Vue-style
    /// state-cycle infinite-loop detection + a per-segment wall-clock watchdog.
    #[serde(default)]
    unbounded_instructions: bool,
    /// Per-segment wall-clock watchdog (ms) when `unbounded_instructions` is enabled.
    #[serde(default)]
    segment_wall_limit_ms: u64,
    #[serde(skip)]
    segment_started_at: Option<std::time::Instant>,
    /// Tracks backward-jump iteration counts and recent state fingerprints per
    /// `(function, pc)` to detect stuck infinite loops in milliseconds.
    #[serde(skip)]
    loop_guard: BTreeMap<(String, usize), LoopGuardEntry>,
    #[serde(default)]
    next_segment: u64,
}

#[derive(Clone, Debug, Default)]
struct LoopGuardEntry {
    hits: u64,
    last_fingerprint: Option<u64>,
    unchanged_samples: u8,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ActionScope {
    /// Live lexical + local JS scope captured when suspending a `@virtual` call/task.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variables: BTreeMap<String, Value>,
    /// Required host libraries reachable from the call arguments or captured scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub libraries: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub at: Location,
    pub destination: Option<String>,
    pub kind: ActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_scope: Option<Box<ActionScope>>,
}
impl Action {
    pub fn scope_variables(&self) -> Option<&BTreeMap<String, Value>> {
        self.captured_scope.as_deref().map(|s| &s.variables).filter(|v| !v.is_empty())
    }
    #[cfg(test)]
    pub fn scope_libraries(&self) -> &[String] {
        self.captured_scope.as_deref().map(|s| s.libraries.as_slice()).unwrap_or(&[])
    }
}
struct PreparedAction {
    kind: ActionKind,
    captured_scope: Option<Box<ActionScope>>,
}
impl From<ActionKind> for PreparedAction {
    fn from(kind: ActionKind) -> Self {
        Self { kind, captured_scope: None }
    }
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
    /// Call an exported JS function from a required orchestration/skill library.
    LibraryCall {
        library: String,
        method: String,
        args: Vec<Value>,
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
            value => Ok(normalize_tool_value(heap.to_json(value)?)),
        })
        .collect()
}

/// The VM serializes RegExp as `$flow_regex`; io.search accepts its source as
/// query plus the explicit regex switch used by the Rust tool contract.
fn normalize_tool_value(mut value: Value) -> Value {
    if let Value::Object(object) = &mut value {
        if let Some(query) = object.get("query")
            && let Some(regex) = query.get("$flow_regex").and_then(Value::as_object)
            && let Some(pattern) = regex.get("pattern").and_then(Value::as_str)
        {
            let ignore_case = regex
                .get("flags")
                .and_then(Value::as_str)
                .is_some_and(|flags| flags.contains('i'));
            object.insert("query".into(), Value::String(pattern.to_owned()));
            object.insert("regex".into(), Value::Bool(true));
            if ignore_case && !object.contains_key("case_sensitive") {
                object.insert("case_sensitive".into(), Value::Bool(false));
            }
            return value;
        }
        for child in object.values_mut() {
            *child = normalize_tool_value(std::mem::take(child));
        }
    } else if let Value::Array(items) = &mut value {
        for child in items {
            *child = normalize_tool_value(std::mem::take(child));
        }
    }
    value
}

include!("parts/runtime_impl_1.inc.rs");
include!("parts/runtime_impl_2.inc.rs");
include!("parts/runtime_impl_3.inc.rs");
include!("parts/runtime_impl_4.inc.rs");
include!("parts/runtime_impl_5.inc.rs");
