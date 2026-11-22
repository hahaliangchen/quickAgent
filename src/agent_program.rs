//! A resumable JavaScript runtime for executable orchestration.
//! SWC compiles it to serializable control flow; effects use the normal host tools.
mod builtins;
mod compiler;
mod heap;
mod js_methods;
mod js_functions;
mod js_collections;
mod js_date;
mod js_locale;
mod js_value;
mod vm_eval;
pub(crate) mod declarations;
pub(crate) mod libraries;
mod promise;
mod prelude;
mod resource_handle;
pub(crate) mod runtime;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod conformance;
mod values;

#[cfg(test)]
pub use compiler::{compile, compile_body, compile_entry};
#[allow(unused_imports)]
pub use compiler::{compile_body_with_scope, compile_segment, uncalled_functions};
#[cfg(test)]
pub use compiler::compile_with_libraries;
#[allow(unused_imports)]
pub use runtime::{Action, Runtime, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const VERSION: u32 = 14;
pub(super) fn js_prototype_global(name:&str)->bool {
    matches!(name,"Symbol"|"Object"|"Function"|"Array"|"String"|"Number"|"Boolean"|"Date"|"RegExp"|"Promise"|"Map"|"Set"|"WeakMap"|"WeakSet"|"URL"|"Buffer"|"globalThis") || vm_eval::ERROR_CLASSES.contains(&name)
}
/// Id of the function formed by a program's top-level statements. It is not
/// a JavaScript identifier, so no declared function can take it.
pub const ENTRY: &str = "<module>";
/// A module implementation's top-level statements. Each call of the
/// implementation runs them first, in its module scope, then its entry.
pub const MODULE_INIT: &str = "<module-init>";
// JavaScript has one number type; JSON tool schemas often require integer
// representations. Preserve integral values at the deterministic boundary.
fn number_value(value: f64) -> Value {
    if value.fract() == 0.0 && value >= 0.0 && value < (u64::MAX as f64) {
        json!(value as u64)
    } else if value.fract() == 0.0 && value < 0.0 && value >= (i64::MIN as f64) {
        json!(value as i64)
    } else {
        json!(value)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Program {
    pub version: u32,
    pub source: String,
    #[serde(default)]
    pub prelude_hash: String,
    pub functions: BTreeMap<String, Function>,
    /// The source uses CommonJS `module` or `exports` without declaring them:
    /// the runtime provides both, as Node does for every module.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub commonjs: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Function {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub virtual_task: bool,
    /// A `@virtual` function that contains real executable JavaScript before
    /// its trailing AI fallback; enters its JS frame instead of short-circuiting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial_virtual: bool,
    /// Host-resolved first virtual layer: author/reuse its code per contract.
    /// The @module source marker also sets this bit. The second layer
    /// runs a focused AI tool loop instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub module_task: bool,
    /// True only when the source explicitly used `@module` (never `@virtual`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub explicit_module: bool,
    /// Outer variables a virtual function's contract names with `@uses`: its
    /// closure, read in the scope where it is declared when it is called.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
    /// Authorized tools for a second-layer virtual. First-layer declarations
    /// are resolved to code by the host rather than using this tool scope.
    #[serde(default)]
    pub leaf_tools: Option<Vec<String>>,
    /// The third-party libraries the source declaring a `@virtual` requires:
    /// their methods join its tools when it is called.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leaf_libraries: Vec<String>,
    /// The libraries a `@virtual` names with `@uses`: only their instructions
    /// come into its context.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    pub params: Vec<String>,
    /// Ordinary functions expose their actual call arguments through the
    /// array-like `arguments` binding. Arrows inherit the outer binding.
    #[serde(default)]
    pub arguments_object: bool,
    /// Index of the final rest parameter, if present.
    #[serde(default)]
    pub rest_parameter: Option<usize>,
    /// Non-simple parameters initialize in a separate environment.
    #[serde(default)]
    pub parameter_scope: bool,
    #[serde(default)]
    pub parameter_lexical: Vec<String>,
    /// Hoisted `var` names, undefined on entry.
    #[serde(default)]
    pub vars: Vec<String>,
    /// let/const names of the body's top level, in their dead zone on entry.
    #[serde(default)]
    pub lexical: Vec<String>,
    /// The top-level flow: its frame runs in the module environment itself.
    #[serde(default)]
    pub module: bool,
    #[serde(default)]
    pub is_async: bool,
    #[serde(default)]
    pub is_generator: bool,
    #[serde(default)]
    pub lexical_this: bool,
    #[serde(default)]
    pub length: usize,
    #[serde(default)]
    pub constructable: bool,
    /// A named function expression sees its own name inside its body.
    #[serde(default)]
    pub self_name: Option<String>,
    pub code: Vec<Instruction>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instruction {
    pub at: Location,
    pub op: Op,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
    /// Assign a compiler temporary, or an existing variable through the scope chain.
    Set(String, Expr),
    /// Initialize a let/const/var binding and end its dead zone.
    Declare(String, Expr),
    /// Enter a block scope whose let/const names start in their dead zone.
    EnterScope(Vec<String>),
    /// Enter the body environment after non-simple parameter initialization.
    EnterFunctionBody,
    LeaveScope,
    /// Fresh per-iteration copy of the current scope for `for (let …)`.
    CopyScope,
    SetMember { object: Expr, key: Expr, value: Expr },
    Effect(Option<String>, Effect),
    Append {
        target: String,
        items: Vec<Expr>,
        destination: String,
    },
    Await { value: Expr, destination: String },
    /// Parameter initializers run at call time; the body starts on first next().
    GeneratorStart,
    Yield { value: Expr, destination: String, return_target: usize, delegated: bool },
    Return(Expr),
    Branch(Expr, usize),
    Jump(usize),
    ForNext {
        items: String,
        index: String,
        item: String,
        end: usize,
    },
    Catch {
        target: usize,
        name: String,
    },
    EndCatch,
    Throw(Expr),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Expr {
    Undefined,
    Value(Value),
    Utf16(Vec<u16>),
    Variable(String),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Member(Box<Expr>, Box<Expr>),
    // Pure expressions are deterministic. Calls are lowered to durable effects.
    Binary(String, Box<Expr>, Box<Expr>),
    Unary(String, Box<Expr>),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Stringify(Box<Expr>),
    StringMethod(Box<Expr>, String, Vec<Expr>),
    // Preserve the serialized variant name for existing program snapshots;
    // the built-ins are now implemented directly in Rust.
    NodeBuiltin(String, Vec<Expr>),
    Template(Vec<Expr>),
    ParseJson(Box<Expr>),
    RegExp(Vec<Expr>),
    /// A closure over the current scope for the function with this id.
    Function(String),
    /// A built-in function or tool as a value: "String", "Math.max", "tool:read_file".
    Native(String),
    Intrinsic(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Invocation {
    pub callee: Expr,
    pub receiver: Expr,
    pub args: Expr,
    #[serde(default)]
    pub label: Option<Expr>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Effect {
    /// A captured function reference plus its receiver, evaluated before args.
    Invoke(Box<Invocation>),
    Construct(Box<Invocation>),
    PromiseCreate { reject: bool, value: Expr },
    PromiseConstruct { executor: Expr },
    /// `require("ppt")`: the host links a library when this runs.
    Require(String),
    /// Dynamic `require(name)`: evaluate the name, then ask the host to link it.
    RequireValue(Expr),
    Tool(String, Vec<Expr>),
    ParallelReads(Vec<(String, Vec<Expr>)>),
    ParallelReadMap {
        name: String,
        items: Expr,
        parameter: String,
        args: Vec<Expr>,
    },
    Call(String, Vec<Expr>),
    Fork(String, Vec<Expr>),
    Join(Vec<Expr>),
    Task {
        name: String,
        description: String,
        input: Expr,
    },
    /// Call a function value: a closure, a built-in or a tool.
    Dynamic { callee: Expr, args: Vec<Expr> },
    /// Internal prelude call using an array of arguments.
    Apply { callee: Expr, args: Expr },
    /// A method call with a computed name and an array of arguments.
    DynamicMethodApply { receiver: Expr, name: Expr, args: Expr },
    /// A method call whose arguments are supplied in an array.
    MethodApply { receiver: Expr, name: String, args: Expr, #[serde(default)] ignore_property: bool },
    /// `receiver.name(args)`: a function-valued property, else a built-in method.
    Method {
        receiver: Expr,
        name: String,
        args: Vec<Expr>,
    },
}

#[cfg(test)]
pub const SKILL: &str = include_str!("../skills/task-program/SKILL.md");
/// Add a workaround only where an equivalent runtime operation is known.
fn unsupported_builtin(name: &str) -> String {
    let hint = match name {
        "TextEncoder.encodeInto" => " Use new TextEncoder().encode(text) to get a new UTF-8 byte array; in-place encoding is unavailable.",
        "TextDecoder" | "TextDecoder.decode" | "TextDecoderStream" => " UTF-8 decoding from byte arrays is unavailable; Buffer.from only encodes strings here.",
        "Buffer.alloc" | "Buffer.allocUnsafe" => " Buffer objects are unavailable; Buffer.from(text, 'utf8') produces a byte array from a string.",
        "URL.parse" | "URL.canParse" | "url.resolve" => " Use new URL(input, base) to parse or resolve a URL; invalid inputs throw.",
        name if name.starts_with("url.") => " For URL parsing, use new URL(input, base).",
        name if name.starts_with("path.") => " Supported path operations: join, resolve, relative, normalize, dirname, basename, extname, and isAbsolute.",
        "require" | "require.resolve" => " Use require(name) with one library name to load it dynamically.",
        _ => "",
    };
    format!("unsupported built-in {name} in the flow runtime.{hint}")
}
pub const CAPABILITIES: &str = include_str!("../prompts/flow_capabilities.md");
pub const BUILTIN_CONTRACTS: &str = include_str!("../prompts/flow_builtin_contracts.d.ts");

