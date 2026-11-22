use super::*;
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeSet;
use swc_common::{FileName, SourceMap, Span, Spanned, comments::SingleThreadedComments, sync::Lrc};
use swc_ecma_ast as ast;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, lexer::Lexer};
use swc_ecma_visit::{Visit, VisitWith};

#[path="compiler_properties.rs"]
mod properties;
#[path="compiler_iteration.rs"]
mod iteration;
#[path="compiler_classes.rs"]
mod classes;
#[path = "compiler_generators.rs"]
mod generators;

struct Compiler {
    map: Lrc<SourceMap>,
    /// Where the source binds each name, so a rejected declaration is
    /// reported at the name itself rather than at its enclosing body.
    binding_spans: Lrc<BTreeMap<String, Vec<Span>>>,
    names: BTreeSet<String>,
    arities: BTreeMap<String, usize>,
    async_entry: Option<String>,
    libraries: super::libraries::Libraries,
    imports: BTreeMap<String, String>,
    /// Lexical scopes, outermost first: module, function, then blocks. Each
    /// maps a declared name to whether it may be reassigned.
    scopes: Vec<BTreeMap<String, bool>>,
    /// Names inherited from the calling scope of task bodies.
    external: BTreeSet<String>,
    /// Compiler temporaries that rewritten source refers to by name.
    temp_names: BTreeSet<String>,
    /// Block scopes entered at run time at the current point of the code.
    runtime_scopes: usize,
    /// Id of the function being compiled; nested functions extend it.
    function_id: String,
    /// Nested functions compiled so far, including their own nested ones.
    nested: Vec<(String, Function)>,
    /// Top-level @virtual functions: their calls keep exact arity.
    virtuals: BTreeSet<String>,
    /// Top-level async functions, whose calls return a Promise.
    async_functions: BTreeSet<String>,
    async_generator: bool,
    code: Vec<Instruction>,
    temp: usize,
    loops: Vec<LoopContext>,
    handlers: usize,
    finalizers: Vec<FinallyContext>,
    /// Branches that leave the optional chain being compiled.
    chain_exits: Option<Vec<usize>>,
    /// The next lowered expression is the object of the current chain link.
    chain_object: bool,
    /// Header contracts of every function declaration, nested ones included,
    /// keyed by the position of the function: whether it is AI work, whether
    /// it is a @module, and its description.
    contracts: std::sync::Arc<BTreeMap<u32, (bool, bool, String)>>,
    /// The tools a @virtual declared here may use: what this source imports.
    leaf_tools: Option<Vec<String>>,
    /// And the third-party libraries this source requires.
    leaf_libraries: Vec<String>,
    /// Private class names are lowered to per-class collision-free keys.
    private_names: BTreeMap<String, String>,
    /// True only on the top-level module compiler of an appended REPL segment.
    segment_root: bool,
}

/// The library a `require("ppt")` call names: one string argument.
fn required_name(call: &ast::CallExpr) -> Option<String> {
    let [ast::ExprOrSpread { spread: None, expr }] = &call.args[..] else { return None };
    let ast::Expr::Lit(ast::Lit::Str(name)) = &**expr else { return None };
    Some(name.value.to_string_lossy().into_owned())
}
/// A library name, optionally with /promises for its async methods.
fn valid_library(spec: &str) -> bool {
    let name = spec.strip_suffix("/promises").unwrap_or(spec);
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}
/// `host:` is a compatibility alias, independent of import/require syntax.
/// A `node:` prefix is dropped too, so `require("node:fs")` gets the same
/// guidance as `require("fs")` when it is linked.
fn library_specifier(spec: &str) -> Option<&str> {
    let spec = spec.strip_prefix("host:").or_else(|| spec.strip_prefix("node:")).unwrap_or(spec);
    valid_library(spec).then_some(spec)
}
/// Node built-in modules the runtime implements itself, and their functions:
/// `require("path")` gives the same pure operations as the `path` global.
fn node_builtin_module(spec: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match spec.strip_prefix("node:").unwrap_or(spec) {
        "path" => ("path", &["join", "resolve", "relative", "basename", "normalize", "dirname", "extname", "isAbsolute"]),
        "url" => ("url", &["parse"]),
        "os" => ("os", &["homedir", "platform", "tmpdir"]),
        // The npm packages flows reach for most; `yaml` and `js-yaml` spell
        // the same two operations differently.
        "yaml" => ("yaml", &["parse", "stringify"]),
        "js-yaml" => ("yaml", &["load", "dump"]),
        _ => return None,
    })
}
/// The library a `require("ppt")` call names, without /promises.
fn required_library(call: &ast::CallExpr) -> Option<String> {
    let ast::Callee::Expr(callee) = &call.callee else { return None };
    if !matches!(&**callee, ast::Expr::Ident(i) if i.sym == *"require") {
        return None;
    }
    let name = required_name(call)?;
    if node_builtin_module(&name).is_some() {
        return None;
    }
    let spec = library_specifier(&name)?;
    Some(spec.strip_suffix("/promises").unwrap_or(spec).to_owned())
}
/// The libraries a source requires, wherever the calls are, and the
/// variables `const md = require("markdown")` binds to them.
#[derive(Default)]
struct Requires {
    libraries: BTreeSet<String>,
    bindings: BTreeMap<String, String>,
}
impl Visit for Requires {
    fn visit_call_expr(&mut self, call: &ast::CallExpr) {
        if let Some(library) = required_library(call) {
            self.libraries.insert(library);
        }
        call.visit_children_with(self);
    }
    fn visit_var_declarator(&mut self, declarator: &ast::VarDeclarator) {
        if let ast::Pat::Ident(name) = &declarator.name
            && let Some(ast::Expr::Call(call)) = declarator.init.as_deref()
            && let Some(library) = required_library(call)
        {
            self.bindings.insert(name.id.sym.to_string(), library);
        }
        declarator.visit_children_with(self);
    }
}
/// Every name a source binds: declarations, parameters, catch bindings and
/// function names.
struct Bound(BTreeSet<String>, BTreeMap<String, Vec<Span>>);
impl Bound {
    fn bind(&mut self, name: &ast::Ident) {
        self.0.insert(name.sym.to_string());
        self.1.entry(name.sym.to_string()).or_default().push(name.span);
    }
}
impl Visit for Bound {
    fn visit_binding_ident(&mut self, name: &ast::BindingIdent) {
        self.bind(&name.id);
    }
    fn visit_fn_decl(&mut self, f: &ast::FnDecl) {
        self.bind(&f.ident);
        f.visit_children_with(self);
    }
    fn visit_class_decl(&mut self, class: &ast::ClassDecl) {
        self.bind(&class.ident);
        class.visit_children_with(self);
    }
}
/// The libraries a source requires, read without compiling it. A source that
/// does not parse requires nothing.
#[cfg(test)]
pub fn required_libraries(source: &str) -> Vec<String> {
    let map: Lrc<SourceMap> = Default::default();
    let file = map.new_source_file(FileName::Custom("agent-program.js".into()).into(), source.to_owned());
    let lexer = Lexer::new(Syntax::Es(EsSyntax::default()), Default::default(), StringInput::from(&*file), None);
    let Ok(module) = Parser::new_from(lexer).parse_module() else { return Vec::new() };
    let mut requires = Requires::default();
    module.visit_with(&mut requires);
    requires.libraries.into_iter().collect()
}

/// A module probe written as a script, the way one tries something out:
/// top-level statements, perhaps with `await` or a final `return`, and no
/// definition of the function the host calls. Its top-level code becomes the
/// body of `async function entry(params)`, so `await` and `return` mean what
/// they would in that function and the parameters hold this call's
/// arguments. Imports move after the function, where they still hoist; the
/// source keeps its line numbers. None when the source defines `entry`, has
/// no top-level statements to run, or does not parse.
#[cfg(test)]
pub fn probe_as_entry(source: &str, entry: &str, params: &[String]) -> Option<String> {
    let map: Lrc<SourceMap> = Default::default();
    let file = map.new_source_file(FileName::Custom("agent-program.js".into()).into(), source.to_owned());
    let syntax = Syntax::Es(EsSyntax { allow_return_outside_function: true, ..Default::default() });
    let mut parser = Parser::new_from(Lexer::new(syntax, Default::default(), StringInput::from(&*file), None));
    let module = parser.parse_module().ok()?;
    if !parser.take_errors().is_empty() {
        return None;
    }
    let offset = |pos: swc_common::BytePos| (pos - file.start_pos).0 as usize;
    let mut imports = Vec::new();
    let mut runs = false;
    for item in &module.body {
        match item {
            ast::ModuleItem::ModuleDecl(ast::ModuleDecl::Import(import)) => imports.push(offset(import.span.lo)..offset(import.span.hi)),
            ast::ModuleItem::ModuleDecl(_) => return None,
            ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::Fn(function))) => {
                if function.ident.sym == *entry {
                    return None;
                }
            }
            ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::Class(_)) | ast::Stmt::Empty(_)) => {}
            ast::ModuleItem::Stmt(_) => runs = true,
        }
    }
    if !runs {
        return None;
    }
    let mut body = source.to_owned();
    let mut moved = Vec::new();
    for range in imports.iter().rev() {
        moved.push(source[range.clone()].to_owned());
        let blank: String = source[range.clone()].chars().map(|c| if c == '\n' { '\n' } else { ' ' }).collect();
        body.replace_range(range.clone(), &blank);
    }
    moved.reverse();
    Some(format!("async function {entry}({}) {{{body}\n}}\n{}", params.join(", "), moved.join("\n")))
}

/// Global built-in functions that can be used as values, as in `.filter(Boolean)`.
/// `console` methods: output for the AI that wrote the program to read.
const CONSOLE_METHODS: [&str; 6] = ["log", "info", "warn", "error", "debug", "dir"];
const NATIVE_GLOBALS: &[&str] = &[
    "String", "Number", "Boolean", "parseInt", "parseFloat", "isNaN", "isFinite",
    "Symbol", "Array", "Date", "encodeURI", "decodeURI", "encodeURIComponent", "decodeURIComponent", "structuredClone",
    "Map", "Set", "WeakMap", "WeakSet",
    "Object", "Function", "RegExp", "Promise", "URL", "Buffer", "globalThis", "Error", "TypeError", "RangeError", "ReferenceError", "SyntaxError", "EvalError", "URIError", "AggregateError",
    "setTimeout", "setInterval", "clearTimeout", "clearInterval", "queueMicrotask",
];

/// A numeric constant, including NaN and the infinities that JSON cannot hold.
fn number_expr(value: f64) -> Expr {
    if value.is_finite() {
        Expr::Value(number_value(value))
    } else {
        Expr::Unary(
            "+".into(),
            Box::new(Expr::Value(json!(super::js_value::number_to_string(value)))),
        )
    }
}

#[derive(Clone)]
struct FinallyContext {
    block: ast::BlockStmt,
    handlers: usize,
    runtime_scopes: usize,
    scopes_len: usize,
    loops_len: usize,
}

#[derive(Default)]
struct LoopContext {
    breaks: Vec<usize>,
    continues: Vec<usize>,
    handlers: usize,
    scopes: usize,
    is_switch: bool,
    label: Option<String>,
    label_loop: bool,
}

/// Names bound by a declaration pattern, without default-value expressions.
fn pattern_names(pattern: &ast::Pat) -> Vec<String> {
    struct Names(Vec<String>);
    impl Visit for Names {
        fn visit_binding_ident(&mut self, node: &ast::BindingIdent) {
            self.0.push(node.id.sym.to_string());
        }
        fn visit_expr(&mut self, _: &ast::Expr) {}
    }
    let mut names = Names(Vec::new());
    pattern.visit_with(&mut names);
    names.0
}

/// let/const and function declarations directly in a statement list, with
/// whether each may be reassigned. Nested blocks have their own scopes.
fn lexical_names(statements: &[ast::Stmt]) -> Vec<(String, bool)> {
    let mut names = Vec::new();
    for statement in statements {
        match statement {
            ast::Stmt::Decl(ast::Decl::Var(v)) if v.kind != ast::VarDeclKind::Var => {
                for d in &v.decls {
                    for name in pattern_names(&d.name) {
                        names.push((name, v.kind == ast::VarDeclKind::Let));
                    }
                }
            }
            ast::Stmt::Decl(ast::Decl::Fn(f)) => names.push((f.ident.sym.to_string(), true)),
            ast::Stmt::Decl(ast::Decl::Class(c)) => names.push((c.ident.sym.to_string(), true)),
            _ => {}
        }
    }
    names
}

/// The parts of a function, arrow or method that the compiler needs.
struct FunctionSource<'a> {
    /// Its own name, used in its id and, for a named function expression,
    /// visible inside its body.
    name: Option<String>,
    self_name: Option<String>,
    params: Vec<&'a ast::Pat>,
    body: FunctionBody<'a>,
    is_async: bool,
    is_generator: bool,
    lexical_this: bool,
    constructable: bool,
    span: Span,
}
enum FunctionBody<'a> {
    Block(&'a ast::BlockStmt),
    Expr(&'a ast::Expr),
    Missing,
    ClassElements(Vec<ClassElement<'a>>),
}
enum ClassElement<'a> {
    Field { span: Span, key: Expr, hint: Expr, value: Option<&'a ast::Expr>, private: bool },
    Private { span: Span, key: Expr, kind: &'static str, value: Expr, setter: Expr },
    Block { span: Span, function: Expr },
}
impl<'a> FunctionSource<'a> {
    fn method(f:&'a ast::Function,name:Option<String>)->Self {let mut source=Self::function(f,name,None);source.constructable=false;source}
    fn function(f: &'a ast::Function, name: Option<String>, self_name: Option<String>) -> Self {
        Self {
            name,
            self_name,
            params: f.params.iter().map(|p| &p.pat).collect(),
            body: match &f.body {
                Some(body) => FunctionBody::Block(body),
                None => FunctionBody::Missing,
            },
            is_async: f.is_async,
            is_generator: f.is_generator,
            lexical_this: false,
            constructable: !f.is_async && !f.is_generator,
            span: f.span,
        }
    }
    fn arrow(a: &'a ast::ArrowExpr) -> Self {
        Self {
            name: None,
            self_name: None,
            params: a.params.iter().collect(),
            body: match &*a.body {
                ast::BlockStmtOrExpr::BlockStmt(body) => FunctionBody::Block(body),
                ast::BlockStmtOrExpr::Expr(body) => FunctionBody::Expr(body),
            },
            is_async: a.is_async,
            is_generator: a.is_generator,
            lexical_this: true,
            constructable: false,
            span: a.span,
        }
    }
}

/// var names hoisted to the enclosing function, including those in nested
/// blocks and loop heads but not in nested functions.
fn var_names(statements: &[ast::Stmt]) -> Vec<String> {
    struct Vars(Vec<String>);
    impl Visit for Vars {
        fn visit_var_decl(&mut self, node: &ast::VarDecl) {
            if node.kind == ast::VarDeclKind::Var {
                for d in &node.decls {
                    for name in pattern_names(&d.name) {
                        if !self.0.contains(&name) {
                            self.0.push(name);
                        }
                    }
                }
            }
        }
        fn visit_function(&mut self, _: &ast::Function) {}
        fn visit_arrow_expr(&mut self, _: &ast::ArrowExpr) {}
        fn visit_class(&mut self, _: &ast::Class) {}
    }
    let mut vars = Vars(Vec::new());
    for statement in statements {
        statement.visit_with(&mut vars);
    }
    vars.0
}
include!("parts/compiler_impl_1.inc.rs");
include!("parts/compiler_impl_2.inc.rs");

#[cfg(test)]
pub fn compile(source: &str) -> Result<Program> {
    compile_with_libraries(source, super::libraries::builtin())
}
#[cfg(test)]
pub(super) fn compile_conformance(source: &str) -> Result<Program> {
    let names=["assert","__test262CheckThrown","$DONE","compareArray"]
        .into_iter().map(str::to_owned).collect();
    link_prelude(compile_inner(source,super::ENTRY,false,super::libraries::Libraries::new(),names,false,false)?)
}

#[cfg(test)]
pub(super) fn script_parse_error(source: &str) -> bool {
    let map:Lrc<SourceMap>=Default::default();
    let file=map.new_source_file(FileName::Custom("test262.js".into()).into(),source.to_owned());
    let mut parser=Parser::new_from(Lexer::new(Syntax::Es(EsSyntax::default()),Default::default(),StringInput::from(&*file),None));
    parser.parse_script().is_err() || !parser.take_errors().is_empty()
}
#[cfg(test)]
pub fn compile_body(source: &str, entry: &str) -> Result<Program> {
    compile_body_with_libraries(source, entry, super::libraries::builtin())
}
#[cfg(test)]
pub fn compile_with_libraries(
    source: &str,
    libraries: super::libraries::Libraries,
) -> Result<Program> {
    link_prelude(compile_inner(source, super::ENTRY, false, libraries, BTreeSet::new(), false, false)?)
}

/// Compile one top-level REPL segment.  A segment is a module entry whose
/// lexical environment is supplied by the caller, so declarations can be
/// appended to a live Runtime without wrapping them in a short-lived helper
/// function.  The entry name is intentionally supplied by the caller to keep
/// segment ids stable in the context document.
pub fn compile_segment(
    source: &str,
    entry: &str,
    libraries: super::libraries::Libraries,
    external_names: BTreeSet<String>,
) -> Result<Program> {
    ensure!(!entry.is_empty(), "segment entry must not be empty");
    link_prelude(compile_inner(source, entry, false, libraries, external_names, false, true)?)
}

/// A declared function nothing calls or passes on is work the author meant to
/// run: a one-shot program executes its top-level statements, while a module
/// implementation is entered by its declared capability entry. Name an
/// otherwise unused helper instead of letting the program end having done
/// nothing (2026-11-11: a
/// main() that was never called made eighteen programs finish silently).
/// The host checks what an AI submits; one-shot programs execute their
/// top-level statements directly and return their final value.
///
/// It also names a called function whose body is empty and which carries a
/// contract comment but no @virtual or @module tag: that is AI work with its
/// tag left out, and as ordinary JS it would silently return undefined
/// (2026-11-14: a 20-line contract without @virtual made the first program of
/// a test investigation print `undefined`, and the main AI gave up
/// orchestrating). Ordinary functions are never handed to AI on their own.
pub fn uncalled_functions(source: &str, entry: Option<&str>) -> Result<()> {
    let map: Lrc<SourceMap> = Default::default();
    let file = map.new_source_file(FileName::Custom("agent-program.js".into()).into(), source.to_owned());
    let comments = SingleThreadedComments::default();
    let mut parser = Parser::new_from(Lexer::new(
        Syntax::Es(EsSyntax { allow_return_outside_function: true, ..Default::default() }),
        Default::default(),
        StringInput::from(&*file),
        Some(&comments),
    ));
    let Ok(module) = parser.parse_module() else { return Ok(()) };
    /// Name, function span and body span of a declaration whose body holds no statement.
    type Empty = (String, Span, Span);
    struct Scan { declared: Vec<(String, Span)>, empty: Vec<Empty>, exported: BTreeSet<String>, uses: BTreeMap<String, usize> }
    impl Visit for Scan {
        fn visit_fn_decl(&mut self, node: &ast::FnDecl) {
            self.declared.push((node.ident.sym.to_string(), node.ident.span));
            if let Some(body) = &node.function.body
                && body.stmts.iter().all(|s| matches!(s, ast::Stmt::Empty(_)))
            {
                self.empty.push((node.ident.sym.to_string(), node.function.span, body.span));
            }
            node.visit_children_with(self);
        }
        fn visit_export_decl(&mut self, node: &ast::ExportDecl) {
            if let ast::Decl::Fn(function) = &node.decl { self.exported.insert(function.ident.sym.to_string()); }
            node.visit_children_with(self);
        }
        fn visit_ident(&mut self, node: &ast::Ident) {
            *self.uses.entry(node.sym.to_string()).or_default() += 1;
        }
    }
    let mut scan = Scan { declared: Vec::new(), empty: Vec::new(), exported: BTreeSet::new(), uses: BTreeMap::new() };
    module.visit_with(&mut scan);
    let (leading, trailing) = comments.take_all();
    let mut notes = leading.borrow().values().chain(trailing.borrow().values()).flat_map(|v| v.clone()).collect::<Vec<_>>();
    notes.sort_by_key(|c| c.span.lo);
    notes.dedup_by_key(|c| (c.span.lo, c.span.hi));
    let offset = |pos: swc_common::BytePos| (pos.0 - file.start_pos.0) as usize;
    for (name, function, body) in &scan.empty {
        if Some(name.as_str()) == entry || scan.uses.get(name).copied().unwrap_or(0) <= 1 {
            continue;
        }
        // The header is the comments directly above, as compile reads a contract.
        let mut header = Vec::new();
        let mut start = function.lo;
        for note in notes.iter().rev().filter(|n| n.span.hi <= function.lo) {
            if !source.get(offset(note.span.hi)..offset(start)).is_some_and(|gap| gap.trim().is_empty() && gap.matches('\n').count() <= 1) {
                break;
            }
            header.push(note.text.to_string());
            start = note.span.lo;
        }
        header.extend(notes.iter().filter(|n| n.span.lo >= body.lo && n.span.hi <= body.hi).map(|n| n.text.to_string()));
        let contract = header.join("\n");
        let tagged = contract
            .split(|ch: char| !(ch.is_alphanumeric() || matches!(ch, '@' | '_')))
            .any(|word| word == "@virtual" || word == "@module");
        if contract.trim().is_empty() || tagged {
            continue;
        }
        let at = map.lookup_char_pos(function.lo);
        bail!("TS_PROGRAM {}:{}: function {name} has a contract comment and an empty body but no @virtual tag, so calling it would do nothing and return undefined. Add @virtual to its JSDoc to hand this work over, or give it a real body", at.line, at.col.0 + 1);
    }
    for (name, span) in &scan.declared {
        // Its own declaration is one occurrence; any other is a call or a hand-off.
        if Some(name.as_str()) == entry || scan.exported.contains(name) || scan.uses.get(name).copied().unwrap_or(0) > 1 {
            continue;
        }
        let at = map.lookup_char_pos(span.lo);
        let who = match entry {
            Some(entry) => format!("the host calls only {entry}, the implementation entry"),
            None => "the host calls no function for you; a program runs its top-level statements in order".to_owned(),
        };
        bail!("TS_PROGRAM {}:{}: function {name} is declared but never called, so it would never run: {who}. Call it where it should run (for a main() helper, add `await main();` at the top level) or remove it", at.line, at.col.0 + 1);
    }
    Ok(())
}
#[cfg(test)]
pub fn compile_body_with_libraries(
    source: &str,
    entry: &str,
    libraries: super::libraries::Libraries,
) -> Result<Program> {
    compile_body_with_scope(source, entry, libraries, BTreeSet::new())
}
pub fn compile_body_with_scope(
    source: &str,
    entry: &str,
    libraries: super::libraries::Libraries,
    external_names: BTreeSet<String>,
) -> Result<Program> {
    let program = link_prelude(compile_inner(source, entry, true, libraries, external_names, false, false)?)?;
    ensure!(
        !program.functions[entry].virtual_task,
        "the implementation of {entry} needs a real body; its original contract annotation is optional, while empty helpers declare AI work"
    );
    Ok(program)
}
#[cfg(test)]
pub fn compile_entry(source: &str, entry: &str) -> Result<Program> {
    compile_entry_with_libraries(source, entry, super::libraries::builtin())
}
#[cfg(test)]
pub fn compile_entry_with_libraries(
    source: &str,
    entry: &str,
    libraries: super::libraries::Libraries,
) -> Result<Program> {
    link_prelude(compile_inner(source, entry, false, libraries, BTreeSet::new(), false, false)?)
}
fn compile_inner(
    source: &str,
    entry: &str,
    refinement: bool,
    libraries: super::libraries::Libraries,
    external_names: BTreeSet<String>,
    internal: bool,
    segment: bool,
) -> Result<Program> {
    ensure!(
        source.len() <= 100_000,
        "TS_PROGRAM source exceeds 100000 bytes"
    );
    let map: Lrc<SourceMap> = Default::default();
    let file = map.new_source_file(
        FileName::Custom("agent-program.js".into()).into(),
        source.to_owned(),
    );
    let comments = SingleThreadedComments::default();
    let lexer = Lexer::new(
        Syntax::Es(EsSyntax { allow_return_outside_function: !refinement, ..Default::default() }),
        Default::default(),
        StringInput::from(&*file),
        Some(&comments),
    );
    let mut parser = Parser::new_from(lexer);
    // Parser errors are named, not explained; the ones a flow meets get words.
    let describe = |kind: String| -> String {
        match kind.as_str() {
            "ReturnNotAllowed" if refinement => format!(
                "return is only allowed inside a function; the host calls {entry} itself with this call's arguments, so put this code inside {entry}"
            ),
            "ReturnNotAllowed" => "top-level return is supported only by a one-shot program; use it to deliver the final structured result".to_owned(),
            _ => kind,
        }
    };
    let format_snippet = |line_no: usize, col_1based: usize| -> String {
        let Some(line_text) = source.lines().nth(line_no.saturating_sub(1)) else {
            return String::new();
        };
        let chars: Vec<char> = line_text.chars().collect();
        if chars.is_empty() {
            return String::new();
        }
        let col0 = col_1based.saturating_sub(1).min(chars.len());
        let start = col0.saturating_sub(45);
        let end = (col0 + 35).min(chars.len());
        let prefix = if start > 0 { "..." } else { "" };
        let suffix = if end < chars.len() { "..." } else { "" };
        let window: String = chars[start..end].iter().collect();
        let caret_offset = prefix.len() + (col0 - start);
        format!("\n  {prefix}{window}{suffix}\n  {}^", " ".repeat(caret_offset))
    };
    let template_backtick_hint = |line_1based: usize| -> &'static str {
        let lines: Vec<&str> = source.lines().collect();
        let idx = line_1based.saturating_sub(1);
        let current_has_backtick = lines.get(idx).is_some_and(|l| l.contains('`'));
        let prior_opens_template = lines
            .get(..idx)
            .is_some_and(|prev| prev.iter().any(|l| l.contains("String.raw`") || l.contains('`')));
        if current_has_backtick && prior_opens_template {
            "\n  Hint: an unescaped backtick (`) or ${ inside a template literal / String.raw terminates it early. For multi-line code or docs containing backticks, use a quoted line array `[\"line 1\", String.raw`...`].join(\"\\n\")`."
        } else {
            ""
        }
    };
    let module = parser.parse_module().map_err(|e| {
        let p = map.lookup_char_pos(e.span().lo);
        let col = p.col.0 + 1;
        anyhow::anyhow!(
            "TS_PROGRAM {}:{}: {}{}{}",
            p.line,
            col,
            describe(format!("{:?}", e.kind())),
            format_snippet(p.line, col),
            template_backtick_hint(p.line),
        )
    })?;
    if let Some(e) = parser.take_errors().first() {
        let p = map.lookup_char_pos(e.span().lo);
        let col = p.col.0 + 1;
        bail!(
            "TS_PROGRAM {}:{}: {}{}{}",
            p.line,
            col,
            describe(format!("{:?}", e.kind())),
            format_snippet(p.line, col),
            template_backtick_hint(p.line),
        );
    }
    if !internal {
        struct Reserved(bool);
        impl Visit for Reserved {
            fn visit_ident(&mut self, node: &ast::Ident) {
                if node.sym.starts_with("$$") { self.0 = true; }
            }
        }
        let mut reserved = Reserved(false);
        module.visit_with(&mut reserved);
        ensure!(!reserved.0, "TS_PROGRAM: names beginning with $$ are reserved for the prelude");
    }
    let (leading, trailing) = comments.take_all();
    let mut notes = leading
        .borrow()
        .values()
        .chain(trailing.borrow().values())
        .flat_map(|v| v.clone())
        .collect::<Vec<_>>();
    notes.sort_by_key(|c| c.span.lo);
    notes.dedup_by_key(|c| (c.span.lo, c.span.hi));
    let mut external = external_names;
    let mut declarations = Vec::new();
    let mut top_level = Vec::new();
    let mut imports = BTreeMap::new();
    let mut imported_libraries = super::libraries::Libraries::new();
    let mut imported_names = BTreeSet::new();
    let mut has_import = false;
    // Every function declaration, nested ones included, for their contracts.
    struct Declarations(Vec<ast::FnDecl>);
    impl Visit for Declarations {
        fn visit_fn_decl(&mut self, node: &ast::FnDecl) {
            self.0.push(node.clone());
            node.visit_children_with(self);
        }
    }
    let mut all_declarations = Declarations(Vec::new());
    module.visit_with(&mut all_declarations);
    let mut requires = Requires::default();
    module.visit_with(&mut requires);
    let mut bound = Bound(BTreeSet::new(), BTreeMap::new());
    module.visit_with(&mut bound);
    let binding_spans = Lrc::new(std::mem::take(&mut bound.1));
    // Node gives every module `module` and `exports`; so does the runtime
    // when the source uses them without declaring its own.
    struct CommonJs(bool);
    impl Visit for CommonJs {
        fn visit_ident(&mut self, node: &ast::Ident) {
            if matches!(node.sym.as_ref(), "module" | "exports") { self.0 = true; }
        }
    }
    let commonjs = !internal && !bound.0.contains("module") && !bound.0.contains("exports") && {
        let mut found = CommonJs(false);
        module.visit_with(&mut found);
        found.0
    };
    if commonjs {
        external.insert("module".into());
        external.insert("exports".into());
    }
    // Main programs are one-shot top-level scripts. Module implementations
    // still compile against their named entry function.
    let implicit_entry = (!refinement && entry == super::ENTRY) || segment;
    for item in module.body {
        match item {
            ast::ModuleItem::ModuleDecl(ast::ModuleDecl::Import(import)) => {
                has_import = true;
                let module_name = import.src.value.to_string_lossy();
                let Some(namespace) = library_specifier(&module_name) else {
                    bail!("TS_PROGRAM 1:1: invalid library specifier {module_name}");
                };
                let (library, asynchronous) = namespace.strip_suffix("/promises").map(|base| (base, true)).unwrap_or((namespace, false));
                let Some(base_methods) = libraries.get(library) else {
                    if super::libraries::builtin().contains_key(library) {
                        bail!("TS_PROGRAM 1:1: SDK_LIBRARY_UNAVAILABLE: {library} is not exposed in this execution scope or permission mode; check read_tool_contracts. Import and require use the same permissions; changing syntax cannot grant access. Shell methods are available only in deterministic module implementations.");
                    }
                    bail!("TS_PROGRAM 1:1: library {namespace} is unavailable for static import in this scope; check read_tool_contracts for available methods. Third-party libraries load at runtime with const {library} = require(\"{namespace}\")");
                };
                let methods: BTreeMap<String, String> = base_methods.iter().map(|(name, tool)| (name.clone(), if asynchronous { format!("async-tool:{tool}") } else { tool.clone() })).collect();
                ensure!(
                    !import.type_only && !import.specifiers.is_empty(),
                    "TS_PROGRAM 1:1: import tool methods or a namespace"
                );
                for spec in import.specifiers {
                    let (local, method) = match spec {
                        ast::ImportSpecifier::Named(named) => {
                            ensure!(
                                !named.is_type_only,
                                "TS_PROGRAM 1:1: type imports are unsupported"
                            );
                            let method = match named.imported {
                                Some(ast::ModuleExportName::Ident(i)) => i.sym.to_string(),
                                Some(ast::ModuleExportName::Str(s)) => {
                                    s.value.to_string_lossy().into_owned()
                                }
                                None => named.local.sym.to_string(),
                            };
                            (named.local.sym.to_string(), Some(method))
                        }
                        ast::ImportSpecifier::Namespace(ns) => (ns.local.sym.to_string(), None),
                        ast::ImportSpecifier::Default(default) => (default.local.sym.to_string(), None),
                    };
                    ensure!(
                        !super::libraries::reserved(&local) && imported_names.insert(local.clone()),
                        "TS_PROGRAM 1:1: duplicate or reserved import {local}"
                    );
                    if let Some(method) = method {
                        let tool = methods.get(&method).with_context(|| {
                            format!("TS_PROGRAM 1:1: unknown tool {namespace}.{method}")
                        })?;
                        imports.insert(local, tool.clone());
                    } else {
                        imported_libraries.insert(local, methods.clone());
                    }
                }
            }
            ast::ModuleItem::Stmt(ast::Stmt::Decl(ast::Decl::Fn(f))) => declarations.push(f),
            ast::ModuleItem::Stmt(stmt) if refinement || implicit_entry => top_level.push(stmt),
            other => {
                let p = map.lookup_char_pos(other.span().lo);
                bail!(
                    "TS_PROGRAM {}:{}: unsupported module declaration or statement in function refinement",
                    p.line,
                    p.col.0 + 1
                );
            }
        }
    }
    let imported_tools = imports
        .values()
        .chain(imported_libraries.values().flat_map(|methods| methods.values()))
        .map(|tool| tool.trim_start_matches("async-tool:").to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    // The top-level statements are the program, as in a JavaScript module.
    // An implementation's own top-level statements initialize its module scope.
    let module_init = refinement && !top_level.is_empty();
    if module_init {
        struct TopLevelAwait<'a>(Option<Span>, &'a str, Option<Span>);
        impl Visit for TopLevelAwait<'_> {
            fn visit_await_expr(&mut self, node: &ast::AwaitExpr) { self.0.get_or_insert(node.span); }
            // `if (require.main === module) work()` never runs in the host.
            fn visit_if_stmt(&mut self, node: &ast::IfStmt) {
                let is = |e: &ast::Expr, main: bool| match e {
                    ast::Expr::Member(m) if main => matches!(&*m.obj, ast::Expr::Ident(i) if i.sym == *"require")
                        && matches!(&m.prop, ast::MemberProp::Ident(p) if p.sym == *"main"),
                    ast::Expr::Ident(i) => !main && i.sym == *"module",
                    _ => false,
                };
                let guard = matches!(&*node.test, ast::Expr::Bin(b) if matches!(b.op, ast::BinaryOp::EqEqEq | ast::BinaryOp::EqEq)
                    && ((is(&b.left, true) && is(&b.right, false)) || (is(&b.left, false) && is(&b.right, true))));
                node.test.visit_with(self);
                if !guard { node.cons.visit_with(self); }
                if let Some(alt) = &node.alt { alt.visit_with(self); }
            }
            fn visit_call_expr(&mut self, node: &ast::CallExpr) {
                if matches!(&node.callee, ast::Callee::Expr(e) if matches!(&**e, ast::Expr::Ident(i) if i.sym == *self.1)) {
                    self.2.get_or_insert(node.span);
                }
                node.visit_children_with(self);
            }
            fn visit_for_of_stmt(&mut self, node: &ast::ForOfStmt) {
                if node.is_await { self.0.get_or_insert(node.span); }
                node.visit_children_with(self);
            }
            fn visit_function(&mut self, _: &ast::Function) {}
            fn visit_arrow_expr(&mut self, _: &ast::ArrowExpr) {}
            fn visit_class(&mut self, _: &ast::Class) {}
        }
        let mut found = TopLevelAwait(None, entry, None);
        for stmt in &top_level { stmt.visit_with(&mut found); }
        if let Some(span) = found.2 {
            let p = map.lookup_char_pos(span.lo);
            bail!("TS_PROGRAM {}:{}: the host calls {entry} itself for each request; calling it at the top level would run it twice, so remove this call", p.line, p.col.0 + 1);
        }
        if let Some(span) = found.0 {
            let p = map.lookup_char_pos(span.lo);
            bail!("TS_PROGRAM {}:{}: the host calls {entry} itself with this call's arguments and the top level only declares, so put this await inside {entry}", p.line, p.col.0 + 1);
        }
    }
    // Without imports the host's libraries are in scope by name. A name the
    // source binds itself, as in `const io = require("io")`, is its own.
    let libraries = if has_import {
        imported_libraries
    } else {
        libraries.into_iter().filter(|(name, _)| !bound.0.contains(name)).collect()
    };
    ensure!(
        internal || declarations.len() <= 64,
        "TS_PROGRAM supports at most 64 functions"
    );
    let mut names = BTreeSet::new();
    if implicit_entry {
        names.insert(entry.into());
    }
    for f in &declarations {
        ensure!(
            !imported_names.contains(f.ident.sym.as_ref()),
            "function name conflicts with import {}",
            f.ident.sym
        );
        ensure!(
            !libraries.contains_key(f.ident.sym.as_ref()),
            "function name conflicts with library namespace {}",
            f.ident.sym
        );
        ensure!(
            !matches!(
                f.ident.sym.as_ref(),
                "assert" | "JSON" | "Promise"
            ),
            "reserved function name {}",
            f.ident.sym
        );
        ensure!(
            names.insert(f.ident.sym.to_string()),
            "duplicate function {}",
            f.ident.sym
        );
    }
    ensure!(
        names.contains(entry),
        "TS_PROGRAM 1:1: declare the required entry function {entry}"
    );
    let mut functions = BTreeMap::new();
    let mut arities = declarations
        .iter()
        .map(|f| (f.ident.sym.to_string(), f.function.params.len()))
        .collect::<BTreeMap<_, _>>();
    if implicit_entry {
        arities.insert(entry.into(), 0);
    }
    let async_entry = declarations
        .iter()
        .find(|f| f.ident.sym.as_ref() == entry && f.function.is_async && !f.function.is_generator)
        .map(|f| f.ident.sym.to_string());
    let implementation_entry = declarations.iter()
        .find(|f| refinement && f.ident.sym.as_ref() == entry)
        .map(|f| f.function.span.lo);
    // A leading JSDoc and comments inside the body form the task contract.
    // Walk only adjacent comments: a tag inside the previous function must
    // never attach to the next declaration. Every contract is read first, so
    // calls compiled before a declaration know whether it is AI work.
    let contract_of = |f: &ast::FnDecl| -> Result<(bool, bool, String)> {
        let Some(body) = &f.function.body else {
            return Ok((false, false, String::new()));
        };
        let mut header_start = f.function.span.lo;
        let mut header = Vec::new();
        for note in notes.iter().rev().filter(|n| n.span.hi <= f.function.span.lo) {
            let lo = (note.span.hi.0 - file.start_pos.0) as usize;
            let hi = (header_start.0 - file.start_pos.0) as usize;
            // A blank line ends the header: a comment above it belongs to the code before.
            if !source.get(lo..hi).is_some_and(|gap| gap.trim().is_empty() && gap.matches('\n').count() <= 1) {
                break;
            }
            header.push(note.text.to_string());
            header_start = note.span.lo;
        }
        header.reverse();
        let marked = |text: &str, tag: &str| {
            text.split(|ch: char| !(ch.is_alphanumeric() || matches!(ch, '@' | '_')))
                .any(|word| word == tag)
        };
        let inner = notes
            .iter()
            .filter(|n| n.span.lo >= body.span.lo && n.span.hi <= body.span.hi)
            .map(|n| n.text.to_string())
            .collect::<Vec<_>>();
        // The header marks a function, and so does a marker in a body that
        // holds only comments. A nested helper's marker belongs to it.
        let comment_only = body.stmts.iter().all(|s| matches!(s, ast::Stmt::Empty(_)));
        let (header_text, inner_text) = (header.join("\n"), inner.join("\n"));
        let has = |tag: &str| marked(&header_text, tag) || (comment_only && marked(&inner_text, tag));
        let (hook, module_task) = (has("@virtual"), has("@module"));
        if hook && module_task {
            let p = map.lookup_char_pos(f.function.span.lo);
            bail!("TS_PROGRAM {}:{}: {} is marked both @virtual and @module; a @virtual is AI work done in a loop, a @module is code AI writes once, so pick one", p.line, p.col.0 + 1, f.ident.sym);
        }
        let virtual_task = hook || module_task;
        // An author/fixer is implementing this exact entry, not declaring it
        // for another author. Retaining its contract JSDoc is harmless. Keep
        // empty declarations and all nested AI contracts strict.
        if implementation_entry == Some(f.function.span.lo) && virtual_task && !comment_only {
            return Ok((false, false, String::new()));
        }
        header.extend(inner);
        let contract = header.join("\n");
        let description = contract
            .lines()
            .map(|line| line.trim().trim_start_matches('*').trim())
            .map(|line| {
                if virtual_task {
                    line.replace("@virtual", "").replace("@module", "")
                } else {
                    line.to_owned()
                }
            })
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        Ok((virtual_task, module_task, description))
    };
    let contracts = declarations
        .iter()
        .map(|f| Ok((f.ident.sym.to_string(), contract_of(f)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let contracts_by_position = std::sync::Arc::new(
        all_declarations.0.iter().map(|f| Ok((f.function.span.lo.0, contract_of(f)?))).collect::<Result<BTreeMap<_, _>>>()?,
    );
    // A @virtual is limited to what the source declaring it imports, whether
    // that source is a main program or a module. In an appended REPL segment
    // without explicit imports, pre-injected non-shell libraries are in scope.
    let leaf_tools = if segment && !has_import && requires.libraries.is_empty() {
        Some(
            libraries
                .iter()
                .filter(|(lib, _)| !matches!(lib.as_str(), "process" | "session" | "previous"))
                .flat_map(|(_, methods)| methods.values().cloned())
                .filter(|tool| {
                    !crate::project_process::is_execution(tool)
                        && tool != crate::shell_process::READ_ONLY
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        )
    } else {
        Some(imported_tools.clone())
    };
    let leaf_libraries = requires.libraries.iter().cloned().collect::<Vec<_>>();
    let virtuals = contracts
        .iter()
        .filter(|(_, (virtual_task, _, _))| *virtual_task)
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    let async_functions = declarations
        .iter()
        .filter(|f| f.function.is_async && !f.function.is_generator)
        .map(|f| f.ident.sym.to_string())
        .collect::<BTreeSet<_>>();
    // Top-level declarations form the module scope. The top-level flow runs
    // in it, and every top-level function sees it through its scope chain.
    // Top-level functions are bindings of it too, so they can be used as values.
    let function_bindings = declarations
        .iter()
        .map(|f| (f.ident.sym.to_string(), false))
        .collect::<BTreeMap<_, _>>();
    let mut module_scope = function_bindings.clone();
    if implicit_entry || module_init {
        let init_id = if implicit_entry { entry } else { super::MODULE_INIT };
        let mut module_vars = var_names(&top_level);
        // A segment keeps the value of its last bare expression in the
        // DevTools-style `$_` binding.  It is an ordinary module binding so
        // the next segment can read it without a host round trip.
        if segment {
            for special in ["$_", "\0explicit_return", "\0has_last_expr", "\0expr_sinks"] {
                if !module_vars.iter().any(|name| name == special) {
                    module_vars.push(special.into());
                }
            }
        }
        let module_lexical = lexical_names(&top_level);
        let mut c = Compiler {
            map: map.clone(),
            binding_spans: binding_spans.clone(),
            names: names.clone(),
            arities: arities.clone(),
            async_entry: None,
            libraries: libraries.clone(),
            imports: imports.clone(),
            scopes: vec![BTreeMap::new()],
            external: external.clone(),
            temp_names: BTreeSet::new(),
            runtime_scopes: 0,
            function_id: init_id.into(),
            nested: Vec::new(),
            virtuals: virtuals.clone(),
            async_functions: async_functions.clone(),
            async_generator: false,
            code: Vec::new(),
            temp: 0,
            loops: Vec::new(),
            handlers: 0,
            finalizers: Vec::new(),
            chain_exits: None,
            chain_object: false,
            contracts: contracts_by_position.clone(),
            leaf_tools: leaf_tools.clone(),
            leaf_libraries: leaf_libraries.clone(),
            private_names: BTreeMap::new(),
            segment_root: segment && implicit_entry,
        };
        for name in &module_vars {
            c.declare(name, true, module.span)?;
        }
        for (name, mutable) in &module_lexical {
            c.declare(name, *mutable, module.span)?;
        }
        if let Some(name) = c.scopes[0].keys().find(|name| names.contains(*name)) {
            return c.error(
                module.span,
                &format!("{name} is declared both as a function and as a variable"),
            );
        }
        c.scopes[0].extend(function_bindings.clone());
        module_scope = c.scopes[0].clone();
        let last_idx = top_level.len().saturating_sub(1);
        for (idx, stmt) in top_level.iter().enumerate() {
            if segment && let ast::Stmt::Expr(expr) = stmt {
                let is_query = is_bare_query_expr(&expr.expr);
                if is_query || idx == last_idx {
                    let value = c.lower(&expr.expr)?;
                    let value = c.snapshot(expr.span, value);
                    if is_query {
                        let tmp = c.temp();
                        c.emit(
                            expr.span,
                            Op::Append {
                                target: "\0expr_sinks".into(),
                                items: vec![value.clone()],
                                destination: tmp,
                            },
                        );
                    }
                    if idx == last_idx {
                        c.emit(
                            expr.span,
                            Op::Set("\0has_last_expr".into(), Expr::Value(json!(true))),
                        );
                        c.emit(expr.span, Op::Set("$_".into(), value));
                    }
                } else {
                    c.stmt(stmt)?;
                }
            } else {
                c.stmt(stmt)?;
            }
        }
        c.emit(module.span, Op::Return(Expr::Undefined));
        functions.extend(std::mem::take(&mut c.nested));
        functions.insert(
            init_id.into(),
            Function {
                uses: Vec::new(),
                description: String::new(),
                virtual_task: false,
                partial_virtual: false,
                module_task: false,
                explicit_module: false,
                leaf_tools: None,
                leaf_libraries: Vec::new(),
                skills: Vec::new(),
                params: Vec::new(),
                arguments_object: false,
                rest_parameter: None,
                parameter_scope: false,
                parameter_lexical: Vec::new(),
                vars: module_vars,
                lexical: module_lexical.into_iter().map(|(name, _)| name).collect(),
                module: true,
                is_async: implicit_entry,
                is_generator: false,
                lexical_this: false,
                constructable: false,
                length: 0,
                self_name: None,
                code: c.code,
            },
        );
    }
    for f in declarations {
        let mut c = Compiler {
            map: map.clone(),
            binding_spans: binding_spans.clone(),
            names: names.clone(),
            arities: arities.clone(),
            async_entry: async_entry.clone(),
            libraries: libraries.clone(),
            imports: imports.clone(),
            scopes: vec![module_scope.clone(), BTreeMap::new()],
            external: external.clone(),
            temp_names: BTreeSet::new(),
            runtime_scopes: 0,
            function_id: f.ident.sym.to_string(),
            nested: Vec::new(),
            virtuals: virtuals.clone(),
            async_functions: async_functions.clone(),
            async_generator: f.function.is_async && f.function.is_generator,
            code: Vec::new(),
            temp: 0,
            loops: Vec::new(),
            handlers: 0,
            finalizers: Vec::new(),
            chain_exits: None,
            chain_object: false,
            contracts: contracts_by_position.clone(),
            leaf_tools: leaf_tools.clone(),
            leaf_libraries: leaf_libraries.clone(),
            private_names: BTreeMap::new(),
            segment_root: false,
        };
        let mut params = Vec::new();
        let mut parameter_names = Vec::new();
        let mut parameter_lexical = Vec::new();
        let mut parameter_patterns = Vec::new();
        let patterned = f.function.params.iter().any(|p| !matches!(p.pat, ast::Pat::Ident(_)));
        let rest_parameter = f.function.params.iter().position(|p| matches!(p.pat, ast::Pat::Rest(_)));
        for (index, p) in f.function.params.iter().enumerate() {
            let names = pattern_names(&p.pat);
            for name in &names { c.declare(name, true, p.span)?; }
            parameter_names.extend(names.iter().cloned());
            if let ast::Pat::Ident(i) = &p.pat && !patterned {
                params.push(i.id.sym.to_string());
            } else {
                let argument = format!("$$param{index}");
                params.push(argument.clone());
                parameter_lexical.extend(names);
                let pattern = match &p.pat { ast::Pat::Rest(rest) => &*rest.arg, other => other };
                parameter_patterns.push((pattern, argument));
            }
        }
        let arguments_object = !parameter_names.iter().any(|name| name == "arguments");
        if arguments_object {
            c.declare("arguments", false, f.function.span)?;
        }
        let body = f
            .function
            .body
            .as_ref()
            .context("function body is required")?;
        let (virtual_task, module_task, description) = contracts[f.ident.sym.as_ref()].clone();
        // Parameter patterns are lowered to ordinary positional parameters and
        // rebound in the function scope below. Virtual declarations use the
        // same JavaScript binding rules as ordinary functions.
        for (pattern, argument) in parameter_patterns {
            c.bind_value(pattern, Expr::Variable(argument), true)?;
        }
        if f.function.is_generator {
            if virtual_task { return c.error(f.function.span,"@module and @virtual must not be generators"); }
            c.emit(f.function.span, Op::GeneratorStart);
        }
        let vars: Vec<String> = var_names(&body.stmts).into_iter()
            .filter(|name| patterned || !parameter_names.contains(name)).collect();
        let lexical = lexical_names(&body.stmts);
        let lexical_only = body.stmts.iter().filter(|stmt| matches!(stmt, ast::Stmt::Decl(ast::Decl::Var(v)) if v.kind != ast::VarDeclKind::Var)).cloned().collect::<Vec<_>>();
        if let Some((name, _)) = lexical_names(&lexical_only).iter().find(|(name, _)| parameter_names.contains(name)) {
            return c.error(c.binding_span(name, body.span, 0), &format!("duplicate parameter and lexical binding {name}"));
        }
        if patterned { c.scopes.push(BTreeMap::new()); }
        for name in &vars { c.declare(name, true, body.span)?; }
        for (name, mutable) in &lexical { c.declare(name, *mutable, body.span)?; }
        if patterned { c.emit(body.span, Op::EnterFunctionBody); }
        let (empty, error_placeholder) = block_is_virtual_placeholder(&body);
        let partial_virtual = virtual_task && !module_task && !empty && !error_placeholder;
        let uses = if virtual_task {
            if description.is_empty() {
                return c.error(
                    body.span,
                    "@virtual and @module need a task description and return contract",
                );
            }
            if module_task && !empty && !error_placeholder {
                return c.error(
                    body.span,
                    "@module declares reusable AI work: use an empty body or a sole throw new Error with a literal message",
                );
            }
            let uses = if module_task {
                c.no_uses(&description, f.function.span)?;
                Vec::new()
            } else {
                c.captures(&description, &params, f.function.span)?
            };
            if partial_virtual {
                c.hoist_functions(&body.stmts)?;
                for s in &body.stmts {
                    c.stmt(s)?;
                }
            }
            emit_virtual_task_tail(
                &mut c,
                body.span,
                &f.ident.sym,
                description.clone(),
                &params,
                &uses,
            );
            uses
        } else {
            c.hoist_functions(&body.stmts)?;
            for s in &body.stmts {
                c.stmt(s)?;
            }
            c.emit(body.span, Op::Return(Expr::Undefined));
            Vec::new()
        };
        functions.extend(std::mem::take(&mut c.nested));
        functions.insert(
            f.ident.sym.to_string(),
            Function {
                description,
                virtual_task,
                partial_virtual,
                module_task,
                explicit_module: module_task,
                uses,
                leaf_tools: if virtual_task && !module_task { leaf_tools.clone() } else { None },
                leaf_libraries: if virtual_task && !module_task { leaf_libraries.clone() } else { Vec::new() },
                skills: Vec::new(),
                params,
                arguments_object,
                rest_parameter,
                parameter_scope: patterned,
                parameter_lexical,
                vars,
                lexical: lexical.into_iter().map(|(name, _)| name).collect(),
                module: false,
                is_async: f.function.is_async,
                is_generator: f.function.is_generator,
                lexical_this: false,
                constructable: !f.function.is_async && !f.function.is_generator,
                length: f.function.params.iter().take_while(|p|!matches!(&p.pat,ast::Pat::Assign(_) | ast::Pat::Rest(_))).count(),
                self_name: None,
                code: c.code,
            },
        );
    }
    ensure!(
        functions.len() <= 1024,
        "TS_PROGRAM supports at most 1024 functions, including nested functions and arrows"
    );
    // A hook carries the instructions of a library only when its contract
    // names, with @uses, the variable the library was required into.
    for function in functions.values_mut() {
        if function.virtual_task && !function.module_task {
            function.skills = function.uses.iter().filter_map(|name| requires.bindings.get(name).cloned()).collect::<BTreeSet<_>>().into_iter().collect();
        }
    }
    Ok(Program {
        version: VERSION,
        source: source.into(),
        prelude_hash: String::new(),
        functions,
        commonjs,
    })
}

fn link_prelude(mut program: Program) -> Result<Program> {
    for (name, function) in &super::prelude::program().functions {
        if name.starts_with("$$") { program.functions.insert(name.clone(), function.clone()); }
    }
    program.prelude_hash = super::prelude::hash();
    Ok(program)
}

pub(super) fn compile_prelude() -> Result<Program> {
    compile_inner(super::prelude::SOURCE, "$$map", false, super::libraries::Libraries::new(), BTreeSet::new(), true, false)
}

fn block_is_virtual_placeholder(body: &ast::BlockStmt) -> (bool, bool) {
    let empty = body.stmts.iter().all(|s| matches!(s, ast::Stmt::Empty(_)));
    // A sole `throw new Error("literal")` is a conventional unavailable-implementation
    // sentinel for empty stubs.
    let executable: Vec<_> = body.stmts.iter().filter(|s| !matches!(s, ast::Stmt::Empty(_))).collect();
    let error_placeholder = matches!(executable.as_slice(), [ast::Stmt::Throw(t)]
        if matches!(&*t.arg, ast::Expr::New(n)
            if matches!(&*n.callee, ast::Expr::Ident(i) if i.sym == *"Error")
                && n.args.as_ref().is_some_and(|args| args.len() == 1
                    && args[0].spread.is_none()
                    && matches!(&*args[0].expr, ast::Expr::Lit(ast::Lit::Str(_))))));
    (empty, error_placeholder)
}

fn emit_virtual_task_tail(
    c: &mut Compiler,
    span: Span,
    name: &str,
    description: String,
    params: &[String],
    uses: &[String],
) {
    let input = Expr::Object(
        params
            .iter()
            .chain(uses)
            .map(|p| (p.clone(), Expr::Variable(p.clone())))
            .collect(),
    );
    let dest = c.temp();
    c.emit(
        span,
        Op::Effect(
            Some(dest.clone()),
            Effect::Task {
                name: name.to_owned(),
                description,
                input,
            },
        ),
    );
    c.emit(span, Op::Return(Expr::Variable(dest)));
}

/// Returns `true` if a top-level segment expression statement is a bare query
/// expression whose value should be captured as a dataflow sink (e.g.
/// `ppF.slice(280,305); shellF.slice(780,825);` or `({ a, b })`), excluding
/// assignments, updates, `console.*` prints, and container-mutating calls.
fn is_bare_query_expr(mut expr: &ast::Expr) -> bool {
    while let ast::Expr::Paren(p) = expr {
        expr = &p.expr;
    }
    match expr {
        ast::Expr::Assign(_) | ast::Expr::Update(_) => false,
        ast::Expr::Unary(u) if u.op == ast::UnaryOp::Void => false,
        ast::Expr::Call(call) => {
            if let ast::Callee::Expr(callee) = &call.callee
                && let ast::Expr::Member(member) = &**callee
            {
                if let ast::Expr::Ident(obj) = &*member.obj
                    && obj.sym.as_ref() == "console"
                {
                    return false;
                }
                if let ast::MemberProp::Ident(prop) = &member.prop
                    && matches!(
                        prop.sym.as_ref(),
                        "push"
                            | "pop"
                            | "shift"
                            | "unshift"
                            | "splice"
                            | "sort"
                            | "reverse"
                            | "set"
                            | "add"
                            | "delete"
                            | "clear"
                    )
                {
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}
