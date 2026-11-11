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
        "path" => ("path", &["join", "resolve", "relative", "basename", "normalize", "dirname", "extname"]),
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
const CONSOLE_METHODS: [&str; 5] = ["log", "info", "warn", "error", "debug"];
const NATIVE_GLOBALS: &[&str] = &[
    "String", "Number", "Boolean", "parseInt", "parseFloat", "isNaN", "isFinite",
    "Symbol", "Array", "Date", "encodeURI", "decodeURI", "encodeURIComponent", "decodeURIComponent", "structuredClone",
    "Map", "Set", "WeakMap", "WeakSet",
    "Object", "Function", "RegExp", "Promise", "Error", "TypeError", "RangeError", "ReferenceError", "SyntaxError", "EvalError", "URIError", "AggregateError",
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
impl Compiler {
    fn condition(&mut self, e: &ast::Expr) -> Result<Expr> {
        self.reject_unawaited(e, "using it as a condition")?;
        self.lower(e)
    }
    /// The name of a call known to return a Promise: a top-level async
    /// function or a host:<library>/promises tool not shadowed by a local.
    fn unawaited_call(&self, e: &ast::Expr) -> Option<String> {
        let mut e = e;
        while let ast::Expr::Paren(p) = e { e = &p.expr; }
        let ast::Expr::Call(call) = e else { return None };
        let ast::Callee::Expr(callee) = &call.callee else { return None };
        let shadowed = |name: &str| self.temp_names.contains(name) || self.scopes.iter().skip(1).any(|scope| scope.contains_key(name));
        match &**callee {
            ast::Expr::Ident(i) if !shadowed(i.sym.as_ref()) => {
                let name = i.sym.as_ref();
                let asynchronous = self.async_functions.contains(name)
                    || self.imports.get(name).is_some_and(|tool| tool.starts_with("async-tool:"));
                asynchronous.then(|| name.to_owned())
            }
            ast::Expr::Member(m) => {
                let (ast::Expr::Ident(library), ast::MemberProp::Ident(method)) = (&*m.obj, &m.prop) else { return None };
                if shadowed(library.sym.as_ref()) { return None; }
                let tool = self.libraries.get(library.sym.as_ref())?.get(method.sym.as_ref())?;
                tool.starts_with("async-tool:").then(|| format!("{}.{}", library.sym, method.sym))
            }
            _ => None,
        }
    }
    /// Using an un-awaited Promise as a value is almost always a missing
    /// await, and a Promise is always truthy, so such checks would pass.
    fn reject_unawaited(&self, e: &ast::Expr, usage: &str) -> Result<()> {
        match self.unawaited_call(e) {
            Some(name) => self.error(e.span(), &format!("{name}(...) returns a Promise; await it before {usage}")),
            None => Ok(()),
        }
    }
    /// The direct operands of `e` that must not be un-awaited Promises.
    fn reject_unawaited_operands(&self, e: &ast::Expr) -> Result<()> {
        let promise_method = |prop: &ast::MemberProp| matches!(prop, ast::MemberProp::Ident(p) if matches!(p.sym.as_ref(), "then" | "catch" | "finally"));
        match e {
            ast::Expr::Member(m) if !promise_method(&m.prop) => self.reject_unawaited(&m.obj, "reading its properties"),
            ast::Expr::Call(ast::CallExpr { callee: ast::Callee::Expr(callee), .. }) => match &**callee {
                ast::Expr::Member(m) if !promise_method(&m.prop) => self.reject_unawaited(&m.obj, "calling its methods"),
                _ => Ok(()),
            },
            ast::Expr::OptChain(chain) => match &*chain.base {
                ast::OptChainBase::Member(m) if !promise_method(&m.prop) => self.reject_unawaited(&m.obj, "reading its properties"),
                _ => Ok(()),
            },
            ast::Expr::Bin(b) => {
                self.reject_unawaited(&b.left, "using it in an operator")?;
                self.reject_unawaited(&b.right, "using it in an operator")
            }
            ast::Expr::Unary(u) if u.op != ast::UnaryOp::Void => self.reject_unawaited(&u.arg, "using it in an operator"),
            ast::Expr::Tpl(t) => t.exprs.iter().try_for_each(|e| self.reject_unawaited(e, "putting it in a template string")),
            _ => Ok(()),
        }
    }
    fn logical(&mut self, op: &str, left: Expr, right: &ast::Expr, span: Span) -> Result<Expr> {
        let left = self.snapshot(span, left);
        let result = self.temp();
        self.emit(span, Op::Set(result.clone(), left.clone()));
        let test = if op == "??" {
            Expr::Binary(
                "==".into(),
                Box::new(left),
                Box::new(Expr::Value(Value::Null)),
            )
        } else {
            left
        };
        let branch = self.emit(span, Op::Branch(test, 0));
        let skip = if op == "||" {
            Some(self.emit(span, Op::Jump(0)))
        } else {
            None
        };
        let rhs = self.code.len();
        let value = self.lower(right)?;
        self.emit(span, Op::Set(result.clone(), value));
        let end = self.code.len();
        if let Op::Branch(_, target) = &mut self.code[branch].op {
            *target = if op == "||" { rhs } else { end };
        }
        if let Some(skip) = skip {
            self.code[skip].op = Op::Jump(end);
        }
        Ok(Expr::Variable(result))
    }
    /// `x &&= y` assigns when x is truthy, `x ||= y` when falsy and
    /// `x ??= y` when nullish; the branch skips the assignment otherwise.
    fn logical_assignment_test(op: ast::AssignOp, old: Expr) -> Expr {
        match op {
            ast::AssignOp::AndAssign => old,
            ast::AssignOp::OrAssign => Expr::Unary("!".into(), Box::new(old)),
            _ => Expr::Binary("==".into(), Box::new(old), Box::new(Expr::Value(Value::Null))),
        }
    }
    fn compound_operator(op: ast::AssignOp) -> Option<&'static str> {
        Some(match op {
            ast::AssignOp::AddAssign => "+",
            ast::AssignOp::SubAssign => "-",
            ast::AssignOp::MulAssign => "*",
            ast::AssignOp::DivAssign => "/",
            ast::AssignOp::ModAssign => "%",
            ast::AssignOp::ExpAssign => "**",
            ast::AssignOp::BitAndAssign => "&",
            ast::AssignOp::BitOrAssign => "|",
            ast::AssignOp::BitXorAssign => "^",
            ast::AssignOp::LShiftAssign => "<<",
            ast::AssignOp::RShiftAssign => ">>",
            ast::AssignOp::ZeroFillRShiftAssign => ">>>",
            _ => return None,
        })
    }
    fn local_target(&mut self, name: &str, span: Span) -> Result<()> {
        match self.resolve(name) {
            Some(false) => self.error(span, "cannot rebind a const value"),
            Some(true) => Ok(()),
            None => self.error(
                span,
                "assignment to an undeclared variable; declare it with let, const or var",
            ),
        }
    }
    /// The outer variables a virtual function's contract names with JSDoc
    /// `@uses <name>` tags: its closure. Each must be visible where the function
    /// is declared, as a closure's free variables are in JavaScript.
    fn captures(&self, description: &str, params: &[String], span: Span) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for line in description.lines() {
            let Some(rest) = line.trim().strip_prefix("@uses") else { continue };
            let Some(name) = rest.split_whitespace().next().map(|word| word.trim_matches(|c: char| matches!(c, '`' | '{' | '}' | ','))) else {
                return self.error(span, "@uses names an outer variable: @uses <name> <what it is>");
            };
            if params.iter().any(|param| param == name) {
                return self.error(span, &format!("@uses {name}: {name} is a parameter; a parameter is already in scope"));
            }
            if self.resolve(name).is_none() {
                return self.error(span, &format!("@uses {name}: no variable {name} is in scope where this function is declared"));
            }
            if !names.iter().any(|known| known == name) {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }
    /// A @module is code that later calls reuse, like a JavaScript module: it
    /// closes over nothing of its caller and takes what it needs as parameters.
    fn no_uses(&self, description: &str, span: Span) -> Result<()> {
        if description.lines().any(|line| line.trim().starts_with("@uses")) {
            return self.error(span, "@uses is for @virtual functions; a @module takes what it needs as parameters");
        }
        Ok(())
    }
    /// The innermost binding of `name`: Some(true) if it may be reassigned.
    fn resolve(&self, name: &str) -> Option<bool> {
        if self.temp_names.contains(name) {
            return Some(true);
        }
        for scope in self.scopes.iter().rev() {
            if let Some(mutable) = scope.get(name) {
                return Some(*mutable);
            }
        }
        self.external.contains(name).then_some(true)
    }
    /// Open a block scope for its let/const names; no scope is needed without them.
    fn enter_scope(&mut self, span: Span, names: Vec<(String, bool)>) -> Result<bool> {
        if names.is_empty() {
            return Ok(false);
        }
        self.scopes.push(BTreeMap::new());
        for (name, mutable) in &names {
            self.declare(name, *mutable, span)?;
        }
        self.emit(
            span,
            Op::EnterScope(names.into_iter().map(|(name, _)| name).collect()),
        );
        self.runtime_scopes += 1;
        Ok(true)
    }
    fn leave_scope(&mut self, span: Span, entered: bool) {
        if entered {
            self.emit(span, Op::LeaveScope);
            self.runtime_scopes -= 1;
            self.scopes.pop();
        }
    }
    fn begin_loop(&mut self) {
        self.loops.push(LoopContext {
            handlers: self.handlers,
            scopes: self.runtime_scopes,
            ..Default::default()
        });
    }
    /// break/continue leave the exception handlers and block scopes opened
    /// inside the loop; the loop's own stay.
    fn leave_to_loop(&mut self, span: Span, index: usize, current: (usize, usize)) {
        let context = &self.loops[index];
        let (handlers, scopes) = (context.handlers, context.scopes);
        for _ in handlers..current.0 {
            self.emit(span, Op::EndCatch);
        }
        for _ in scopes..current.1 {
            self.emit(span, Op::LeaveScope);
        }
    }
    fn end_loop(&mut self, end: usize, next: usize) {
        let context = self.loops.pop().unwrap();
        for at in context.breaks {
            self.code[at].op = Op::Jump(end);
        }
        for at in context.continues {
            self.code[at].op = Op::Jump(next);
        }
    }
    /// Emit finalizers crossed by a return or a jump to an enclosing loop.
    /// The generated code unwinds to the lexical environment of each finally;
    /// compiler state is restored afterwards for the other control-flow paths.
    fn emit_finalizers(&mut self, target_loop: Option<usize>, span: Span) -> Result<(usize, usize)> {
        let original_handlers = self.handlers;
        let original_runtime_scopes = self.runtime_scopes;
        let mut removed = Vec::new();
        while self.finalizers.last().is_some_and(|ctx| target_loop.is_none_or(|target| target < ctx.loops_len)) {
            let ctx = self.finalizers.pop().unwrap();
            for _ in ctx.handlers..self.handlers { self.emit(span, Op::EndCatch); }
            for _ in ctx.runtime_scopes..self.runtime_scopes { self.emit(span, Op::LeaveScope); }
            self.handlers = ctx.handlers;
            self.runtime_scopes = ctx.runtime_scopes;
            let scopes = self.scopes.split_off(ctx.scopes_len);
            let loops = self.loops.split_off(ctx.loops_len);
            self.stmt(&ast::Stmt::Block(ctx.block.clone()))?;
            removed.push((ctx, scopes, loops));
        }
        let remaining = (self.handlers, self.runtime_scopes);
        while let Some((ctx, scopes, loops)) = removed.pop() {
            self.scopes.extend(scopes);
            self.loops.extend(loops);
            self.finalizers.push(ctx);
        }
        self.handlers = original_handlers;
        self.runtime_scopes = original_runtime_scopes;
        Ok(remaining)
    }

    /// Names are bound when their scope is entered; a declaration initializes
    /// a let/const and assigns a hoisted var.
    fn variable_declaration(&mut self, v: &ast::VarDecl) -> Result<()> {
        let declare = v.kind != ast::VarDeclKind::Var;
        for d in &v.decls {
            match (&d.init, &d.name) {
                (Some(e), _) => self.binding(&d.name, e, declare)?,
                (None, _) if v.kind == ast::VarDeclKind::Const => {
                    return self.error(d.span, "const needs an initializer");
                }
                (None, ast::Pat::Ident(i)) => {
                    // A repeated `var x;` keeps the current value.
                    if declare {
                        self.emit(d.span, Op::Declare(i.id.sym.to_string(), Expr::Undefined));
                    }
                }
                (None, _) => return self.error(d.span, "destructuring needs an initializer"),
            }
        }
        Ok(())
    }
    fn at(&self, span: Span) -> Location {
        let p = self.map.lookup_char_pos(span.lo);
        Location {
            line: p.line,
            column: p.col.0 + 1,
        }
    }
    fn error<T>(&self, span: Span, text: &str) -> Result<T> {
        let p = self.at(span);
        bail!("TS_PROGRAM {}:{}: {text}", p.line, p.column)
    }
    fn emit(&mut self, span: Span, mut op: Op) -> usize {
        self.property_op(span,&mut op);self.raw_emit(span,op)
    }
    fn temp(&mut self) -> String {
        self.temp += 1;
        format!("$temp{}", self.temp)
    }
    // Freeze each argument/field before lowering later effectful expressions.
    // JSON values are snapshots, not references that later push() can change.
    fn snapshot(&mut self, span: Span, value: Expr) -> Expr {
        let name = self.temp();
        self.emit(span, Op::Set(name.clone(), value));
        Expr::Variable(name)
    }
    /// Bind `name` in the innermost scope.
    fn declare(&mut self, name: &str, mutable: bool, span: Span) -> Result<()> {
        let reserved = if matches!(name, "assert" | "JSON" | "Promise") {
            Some(format!("{name} is a host built-in and cannot be declared again; use another name"))
        } else if name.starts_with("$temp") {
            Some(format!("{name}: names beginning with $temp are reserved for the compiler"))
        } else if self.libraries.contains_key(name) {
            Some(format!("{name} is an imported library namespace here; give the local another name"))
        } else if self.imports.contains_key(name) {
            Some(format!("{name} is an imported tool here; give the local another name"))
        } else {
            None
        };
        if let Some(reason) = reserved {
            return self.error(self.binding_span(name, span, 0), &reason);
        }
        let scope = self.scopes.last_mut().expect("a scope is open");
        if scope.contains_key(name) {
            return self.error(self.binding_span(name, span, 1), &format!("{name} is already declared in this scope"));
        }
        scope.insert(name.into(), mutable);
        Ok(())
    }
    /// The `nth` place inside `within` that binds `name`, else the first, else `within`.
    fn binding_span(&self, name: &str, within: Span, nth: usize) -> Span {
        let inside = self.binding_spans.get(name).into_iter().flatten()
            .filter(|span| span.lo >= within.lo && span.hi <= within.hi).copied().collect::<Vec<_>>();
        inside.get(nth).or(inside.first()).copied().unwrap_or(within)
    }
    fn property(&self, p: &ast::PropName) -> Result<String> {
        match p {
            ast::PropName::Ident(i) => Ok(i.sym.to_string()),
            ast::PropName::Str(s) => Ok(s.value.to_string_lossy().into_owned()),
            ast::PropName::Num(n)=>Ok(super::js_value::number_to_string(n.value)),
            _ => self.error(p.span(), "use a static object key"),
        }
    }
    fn private_key(&self, name: &ast::PrivateName) -> Result<Expr> {
        self.private_names
            .get(name.name.as_ref())
            .cloned()
            .map(Expr::Variable)
            .with_context(|| format!("unknown private name #{}", name.name))
    }
    fn expr(&self, e: &ast::Expr) -> Result<Expr> {
        use ast::Expr as E;
        if let Some(native) = self.native_value(e)? {
            return Ok(native);
        }
        Ok(match e {
            E::Lit(ast::Lit::Str(s)) => match s.value.as_str() {
                Some(text) => Expr::Value(json!(text)),
                None => Expr::Utf16(s.value.as_wtf8().to_ill_formed_utf16().collect()),
            },
            E::Lit(ast::Lit::Regex(r)) => {
                let pattern = r.exp.to_string();
                let flags = r.flags.to_string();
                super::values::compile_regex(&pattern, &flags).map_err(|e| {
                    let p = self.at(r.span);
                    anyhow::anyhow!(
                        "TS_PROGRAM {}:{}: invalid regular expression: {e}",
                        p.line,
                        p.column
                    )
                })?;
                Expr::Value(json!({"$flow_regex": {"pattern": pattern, "flags": flags}}))
            }
            E::Lit(ast::Lit::Num(n)) => {
                if !n.value.is_finite() {
                    return self.error(n.span, "numbers must be finite JSON values");
                }
                if let Some(raw) = &n.raw {
                    let raw = raw.replace('_', "");
                    if raw.chars().all(|c| c.is_ascii_digit()) {
                        return raw
                            .parse::<u64>()
                            .map(|n| Expr::Value(json!(n)))
                            .or_else(|_| {
                                self.error(n.span, "integer literal exceeds the JSON integer range")
                            });
                    }
                    if ["0x", "0X", "0o", "0O", "0b", "0B"]
                        .iter()
                        .any(|prefix| raw.starts_with(prefix))
                        && n.value.abs() > 9_007_199_254_740_991.0
                    {
                        return self.error(n.span, "large integers must use decimal notation");
                    }
                }
                Expr::Value(number_value(n.value))
            }
            E::Lit(ast::Lit::Bool(b)) => Expr::Value(json!(b.value)),
            E::Lit(ast::Lit::Null(_)) => Expr::Value(Value::Null),
            E::Ident(i) if self.resolve(i.sym.as_ref()).is_some() => {
                Expr::Variable(i.sym.to_string())
            }
            E::Ident(i) if i.sym == *"undefined" => Expr::Undefined,
            E::Ident(i) if matches!(i.sym.as_ref(), "NaN" | "Infinity") => {
                number_expr(if i.sym == *"NaN" { f64::NAN } else { f64::INFINITY })
            }
            E::Member(m) if self.constant_member(m).is_some() => self.constant_member(m).unwrap(),
            // A host program is never Node's main module, so the usual
            // `if (require.main === module) main()` guard stays closed.
            E::Member(m) if matches!(&*m.obj, ast::Expr::Ident(i) if i.sym == *"require") && self.global("require")
                && matches!(&m.prop, ast::MemberProp::Ident(p) if p.sym == *"main") => Expr::Undefined,
            E::Ident(i) => {
                if super::vm_eval::ERROR_CLASSES.contains(&i.sym.as_ref()) {
                    return self.error(i.span,&format!("{} 作为函数值在当前流程运行时里不可用；构造调用 {}(...) / new {}(...) 可用",i.sym,i.sym,i.sym));
                }
                return self.error(
                    e.span(),
                    &format!("unknown variable {}: declare it in this function or at the top level, or pass it as a parameter",i.sym),
                );
            }
            E::Paren(p) => return self.expr(&p.expr),
            E::Array(a) => Expr::Array(
                a.elems
                    .iter()
                    .map(|x| match x {
                        Some(x) if x.spread.is_none() => self.expr(&x.expr),
                        _ => self.error(e.span(), "array holes and spreads are unsupported"),
                    })
                    .collect::<Result<_>>()?,
            ),
            E::Object(o) => Expr::Object(
                o.props
                    .iter()
                    .map(|p| match p {
                        ast::PropOrSpread::Prop(p) => match &**p {
                            ast::Prop::KeyValue(p) => {
                                Ok((self.property(&p.key)?, self.expr(&p.value)?))
                            }
                            ast::Prop::Shorthand(i) => {
                                Ok((i.sym.to_string(), self.expr(&E::Ident(i.clone()))?))
                            }
                            _ => self.error(p.span(), "object methods/accessors are unsupported"),
                        },
                        _ => self.error(e.span(), "object spread is unsupported"),
                    })
                    .collect::<Result<_>>()?,
            ),
            E::Member(m) => Expr::Member(
                Box::new(self.expr(&m.obj)?),
                Box::new(match &m.prop {
                    ast::MemberProp::Ident(i) => Expr::Value(json!(i.sym.to_string())),
                    ast::MemberProp::PrivateName(i) => self.private_key(i)?,
                    ast::MemberProp::Computed(c) => self.expr(&c.expr)?,
                }),
            ),
            E::Assign(_) => {
                return self.error(
                    e.span(),
                    "assignment is only allowed as a standalone local statement",
                );
            }
            E::Cond(_) | E::Tpl(_) => {
                return self.error(
                    e.span(),
                    "templates and ternaries are unsupported; use a task helper and if/else",
                );
            }
            E::Update(_) | E::TsNonNull(_) => {
                return self.error(
                    e.span(),
                    "mutation operators and type assertions are unsupported",
                );
            }
            E::Call(_) => {
                return self.error(
                    e.span(),
                    "calls must be lowered through the task/tool or semantic boundary",
                );
            }
            E::New(new) => {
                let name=match &*new.callee {E::Ident(i)=>i.sym.to_string(),_=>"表达式构造器".into()};
                let alternative=match name.as_str() {
                    "Map" | "WeakMap"=>"；当前可用普通对象保存字符串键，但不能替代对象键和引用身份语义",
                    "Set" | "WeakSet"=>"；当前可用数组配合 includes 处理小集合，但不能替代全部集合语义",
                    _=>"",
                };
                return self.error(new.span,&format!("new {name}(...) 构造在流程运行时里尚不支持{alternative}。{}", crate::agent_program::NOT_AVAILABLE_HINT));
            }
            E::This(_)=>Expr::Variable("\0this".into()),
            E::Class(node)=>return self.error(node.class.span,"class 表达式在流程运行时里尚不支持"),
            E::Yield(node)=>return self.error(node.span,"yield must pass through the resumable expression boundary"),
            E::TaggedTpl(node) => return self.error(node.span, "tagged templates must be evaluated as expressions"),
            E::MetaProp(node) if node.kind==ast::MetaPropKind::NewTarget=>Expr::Variable("\0newTarget".into()),
            E::MetaProp(node)=>return self.error(node.span,"import.meta 在流程运行时里尚不支持"),
            _ => {
                return self.error(
                    e.span(),
                    "unsupported JavaScript expression in this VM",
                );
            }
        })
    }
    /// Index of the scope that binds `name`; the module scope is 0.
    fn resolve_level(&self, name: &str) -> Option<usize> {
        self.scopes.iter().rposition(|scope| scope.contains_key(name))
    }
    /// A callee resolved at compile time: an imported tool, a
    /// library method or a top-level function that no inner binding shadows.
    /// Anything else is a function value called at run time.
    fn static_callee(&self, target: &ast::Expr) -> bool {
        match target {
            ast::Expr::Ident(i) => {
                let name = i.sym.as_ref();
                self.imports.contains_key(name)
                    || ((self.virtuals.contains(name) || self.async_entry.as_deref()==Some(name)) && matches!(self.resolve_level(name),Some(0)|None))
                    || (self.resolve(name).is_none() && !self.names.contains(name) && !NATIVE_GLOBALS.contains(&name))
            }
            ast::Expr::Member(m) => matches!(&*m.obj, ast::Expr::Ident(i)
                if self.libraries.contains_key(i.sym.as_ref()) && self.resolve(i.sym.as_ref()).is_none()),
            _ => false,
        }
    }
    /// Call a function value. The callee is evaluated before the arguments.
    fn dynamic_call(&mut self, call: &ast::CallExpr, callee: &ast::Expr) -> Result<Expr> {
        let mut target=callee;
        while let ast::Expr::Paren(p)=target {target=&p.expr;}
        if let ast::Expr::Member(member)=target {
            let receiver=self.lower_object(&member.obj)?;
            let receiver=self.snapshot(member.obj.span(),receiver);
            let key=self.member_key(&member.prop,member.span)?;
            return self.call_reference(receiver,key,&call.args,call.span,false);
        }
        if let ast::Expr::OptChain(chain)=target && let ast::OptChainBase::Member(member)=&*chain.base {
            let receiver=self.temp();
            self.emit(call.span,Op::Set(receiver.clone(),Expr::Undefined));
            let callee=self.with_chain(chain.span,|c| {
                let value=c.lower_object(&member.obj)?;
                let value=c.snapshot(member.obj.span(),value);
                c.emit(member.span,Op::Set(receiver.clone(),value.clone()));
                if chain.optional {c.chain_guard(chain.span,value.clone());}
                let key=c.member_key(&member.prop,member.span)?;
                Ok(Expr::Member(Box::new(value),Box::new(key)))
            })?;
            return self.invoke_reference(callee,Expr::Variable(receiver),&call.args,call.span,None);
        }
        if call.args.iter().any(|a| a.spread.is_some()) {
            return self.error(call.span, "argument spread is unsupported");
        }
        let value = self.lower(callee)?;
        let callee = self.snapshot(callee.span(), value);
        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            let value = self.lower(&arg.expr)?;
            args.push(self.snapshot(arg.span(), value));
        }
        let dest = self.temp();
        self.emit(
            call.span,
            Op::Effect(Some(dest.clone()), Effect::Dynamic { callee, args }),
        );
        Ok(Expr::Variable(dest))
    }
    /// Compile a nested function, arrow or method into its own code unit and
    /// return its id. Its body sees every scope visible at this point.
    fn compile_function(&mut self, source: FunctionSource<'_>) -> Result<String> {
        let at = self.at(source.span);
        let id = format!(
            "{}/{}@{}:{}",
            self.function_id,
            source.name.as_deref().unwrap_or("<anonymous>"),
            at.line,
            at.column
        );
        let mut scopes = self.scopes.clone();
        scopes.push(BTreeMap::new());
        let mut c = Compiler {
            map: self.map.clone(),
            binding_spans: self.binding_spans.clone(),
            names: self.names.clone(),
            arities: self.arities.clone(),
            async_entry: None,
            libraries: self.libraries.clone(),
            imports: self.imports.clone(),
            scopes,
            external: self.external.clone(),
            temp_names: BTreeSet::new(),
            runtime_scopes: 0,
            function_id: id.clone(),
            nested: Vec::new(),
            virtuals: self.virtuals.clone(),
            async_functions: self.async_functions.clone(),
            async_generator: source.is_async && source.is_generator,
            code: Vec::new(),
            temp: 0,
            loops: Vec::new(),
            handlers: 0,
            finalizers: Vec::new(),
            chain_exits: None,
            chain_object: false,
            contracts: self.contracts.clone(),
            leaf_tools: self.leaf_tools.clone(),
            leaf_libraries: self.leaf_libraries.clone(),
            private_names: self.private_names.clone(),
        };
        let mut params = Vec::new();
        let mut parameter_lexical = Vec::new();
        let mut parameter_patterns = Vec::new();
        let mut parameter_names = Vec::new();
        let patterned = source.params.iter().any(|p| !matches!(p, ast::Pat::Ident(_)));
        let rest_parameter = source.params.iter().position(|p| matches!(p, ast::Pat::Rest(_)));
        for (index, pattern) in source.params.iter().enumerate() {
            let names = pattern_names(pattern);
            for name in &names { c.declare(name, true, pattern.span())?; }
            parameter_names.extend(names.iter().cloned());
            if let ast::Pat::Ident(i) = pattern && !patterned {
                params.push(i.id.sym.to_string());
            } else {
                let argument = format!("$$param{index}");
                params.push(argument.clone());
                parameter_lexical.extend(names);
                let pattern = match pattern { ast::Pat::Rest(rest) => &*rest.arg, _ => pattern };
                parameter_patterns.push((pattern, argument));
            }
        }
        let arguments_object = !source.lexical_this && !parameter_names.iter().any(|name| name == "arguments");
        if arguments_object {
            c.declare("arguments", false, source.span)?;
        }
        // The function name is available in parameter initializers.
        if let Some(name) = &source.self_name {
            let scope = c.scopes.last_mut().expect("function scope");
            if !scope.contains_key(name) { scope.insert(name.clone(), false); }
        }
        for (pattern, argument) in parameter_patterns {
            c.bind_value(pattern, Expr::Variable(argument), true)?;
        }
        if source.is_generator { c.emit(source.span, Op::GeneratorStart); }
        let (vars, lexical) = match &source.body {
            FunctionBody::Block(body) => {
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
                (vars, lexical)
            }
            _ => (Vec::new(), Vec::new()),
        };
        // A nested declaration marked @virtual or @module is AI work, as at the top level.
        let contract = source.name.as_ref().and_then(|_| self.contracts.get(&source.span.lo.0)).filter(|(v, _, _)| *v).cloned();
        let module_task = contract.as_ref().is_some_and(|(_, module, _)| *module);
        if source.is_generator && contract.is_some() { return c.error(source.span,"@module and @virtual must not be generators"); }
        match source.body {
            FunctionBody::Block(body) if contract.is_some() => {
                let (_, _, description) = contract.clone().unwrap();
                if patterned {
                    return c.error(source.span, "@virtual and @module functions require simple parameters");
                }
                if !body.stmts.iter().all(|s| matches!(s, ast::Stmt::Empty(_))) {
                    return c.error(body.span, "@virtual and @module declare AI work: the body holds only comments describing it");
                }
                if description.is_empty() {
                    return c.error(body.span, "@virtual and @module need a task description and return contract");
                }
                let uses = if module_task {
                    self.no_uses(&description, source.span)?;
                    Vec::new()
                } else {
                    self.captures(&description, &params, source.span)?
                };
                // Captured variables join the input, read in this closure's scope.
                let input = Expr::Object(params.iter().chain(&uses).map(|p| (p.clone(), Expr::Variable(p.clone()))).collect());
                let dest = c.temp();
                c.emit(body.span, Op::Effect(Some(dest.clone()), Effect::Task { name: id.clone(), description, input }));
                c.emit(body.span, Op::Return(Expr::Variable(dest)));
            }
            FunctionBody::Block(body) => {
                c.hoist_functions(&body.stmts)?;
                for s in &body.stmts {
                    c.stmt(s)?;
                }
                c.emit(body.span, Op::Return(Expr::Undefined));
            }
            FunctionBody::Expr(body) => {
                let value = c.lower(body)?;
                c.emit(body.span(), Op::Return(value));
            }
            FunctionBody::ClassElements(elements) => { c.emit_class_elements(elements)?; c.emit(source.span, Op::Return(Expr::Undefined)); }
            FunctionBody::Missing => return c.error(source.span, "function body is required"),
        }
        self.nested.extend(c.nested);
        let virtual_task = contract.is_some();
        let description = contract.map(|(_, _, description)| description).unwrap_or_default();
        let uses = if virtual_task && !module_task { self.captures(&description, &params, source.span)? } else { Vec::new() };
        self.nested.push((
            id.clone(),
            Function {
                description,
                virtual_task,
                module_task,
                uses,
                leaf_tools: if virtual_task && !module_task { self.leaf_tools.clone() } else { None },
                leaf_libraries: if virtual_task && !module_task { self.leaf_libraries.clone() } else { Vec::new() },
                skills: Vec::new(),
                params,
                arguments_object,
                rest_parameter,
                parameter_scope: patterned,
                parameter_lexical,
                vars,
                lexical: lexical.into_iter().map(|(name, _)| name).collect(),
                module: false,
                is_async: source.is_async,
                is_generator: source.is_generator,
                lexical_this: source.lexical_this,
                constructable: source.constructable,
                length: source.params.iter().take_while(|p|!matches!(p,ast::Pat::Assign(_) | ast::Pat::Rest(_))).count(),
                self_name: source.self_name,
                code: c.code,
            },
        ));
        Ok(id)
    }
    /// Function declarations are initialized as soon as their scope is entered.
    fn hoist_functions(&mut self, statements: &[ast::Stmt]) -> Result<()> {
        for statement in statements {
            if let ast::Stmt::Decl(ast::Decl::Fn(f)) = statement {
                let name = f.ident.sym.to_string();
                let id = self.compile_function(FunctionSource::function(
                    &f.function,
                    Some(name.clone()),
                    None,
                ))?;
                self.emit(f.function.span, Op::Declare(name, Expr::Function(id)));
            }
        }
        Ok(())
    }
    /// A built-in function or tool named where a value is expected, such as
    /// `String`, `Math.max`, an imported tool or `io.read`.
    fn native_value(&self, e: &ast::Expr) -> Result<Option<Expr>> {
        Ok(match e {
            ast::Expr::Ident(i) => {
                let name = i.sym.as_ref();
                if self.resolve(name).is_some() {
                    None
                } else if let Some(tool) = self.imports.get(name) {
                    Some(Expr::Native(format!("tool:{tool}")))
                } else if self.global(name) && (matches!(name,"Math"|"JSON"|"Reflect") || super::js_prototype_global(name)) {
                    Some(Expr::Intrinsic(name.into()))
                } else if self.global(name) && NATIVE_GLOBALS.contains(&name) {
                    Some(Expr::Native(name.to_owned()))
                } else if cfg!(test) && self.external.contains("__test262CheckThrown") && name=="Test262Error" && self.global(name) {
                    Some(Expr::Native("test262.error".into()))
                } else {
                    None
                }
            }
            ast::Expr::Member(m) => {
                let (ast::Expr::Ident(namespace), ast::MemberProp::Ident(property)) = (&*m.obj, &m.prop)
                else {
                    return Ok(None);
                };
                let (namespace, property) = (namespace.sym.as_ref(), property.sym.as_ref());
                if self.resolve(namespace).is_some() {
                    None
                } else if let Some(methods) = self.libraries.get(namespace) {
                    match methods.get(property) {
                        Some(tool) => Some(Expr::Native(format!("tool:{tool}"))),
                        None if namespace == "process" && property == "env" => {
                            return self.error(m.span, "read environment variables one at a time, as process.env.NAME or process.env[\"NAME\"]");
                        }
                        None => {
                            return self.error(
                                m.span,
                                &format!("unknown library method {namespace}.{property}; use the published library catalog"),
                            );
                        }
                    }
                } else if namespace == "console" && self.global("console") && CONSOLE_METHODS.contains(&property) {
                    Some(Expr::Native(format!("console.{property}")))
                } else {
                    let name = format!("{namespace}.{property}");
                    (matches!(namespace, "Math" | "Number" | "String" | "Date" | "Object" | "Array" | "JSON" | "Reflect" | "Symbol")
                        && self.global(namespace)
                        && super::js_methods::builtin_arity(&name).is_some())
                    .then_some(Expr::Member(Box::new(Expr::Intrinsic(namespace.into())),Box::new(Expr::Value(json!(property)))))
                }
            }
            _ => None,
        })
    }
    fn effect(&self, call: &ast::CallExpr) -> Result<Effect> {
        if call.args.iter().any(|a| a.spread.is_some()) {
            return self.error(call.span, "argument spreads are unsupported");
        }
        let ast::Callee::Expr(target) = &call.callee else {
            return self.error(call.span, "use a declared helper or library.method");
        };
        if matches!(&**target, ast::Expr::Ident(i) if matches!(i.sym.as_ref(), "fork" | "join") && !self.names.contains(i.sym.as_ref()) && self.resolve(i.sym.as_ref()).is_none()) {
            return self.error(call.span, "fork/join have been retired; use async virtual functions and Promise.all");
        }
        let actuals = &call.args[..];
        let target = &**target;
        let args = actuals
            .iter()
            .map(|a| self.expr(&a.expr))
            .collect::<Result<Vec<_>>>()?;
        let target = match target {
            ast::Expr::Ident(i) if self.imports.contains_key(i.sym.as_ref()) => {
                self.imports[i.sym.as_ref()].clone()
            }
            ast::Expr::Ident(i) if self.names.contains(i.sym.as_ref()) => {
                if self.async_entry.as_deref() == Some(i.sym.as_ref()) {
                    return self.error(
                        call.span,
                        "the async entry is awaited by the host and cannot be called as a helper",
                    );
                }
                // Virtual calls bind arguments exactly; ordinary functions use JS arity.
                let exact = self.virtuals.contains(i.sym.as_ref());
                if exact && self.arities[i.sym.as_ref()] != args.len() {
                    return self.error(
                        call.span,
                        &format!(
                            "{} expects {} arguments, got {}",
                            i.sym,
                            self.arities[i.sym.as_ref()],
                            args.len()
                        ),
                    );
                }
                return Ok(Effect::Call(i.sym.to_string(), args));
            }
            ast::Expr::Member(m) if matches!(&*m.obj,ast::Expr::Ident(i) if self.libraries.contains_key(i.sym.as_ref())) =>
            {
                let ast::Expr::Ident(library) = &*m.obj else {
                    unreachable!()
                };
                let ast::MemberProp::Ident(method) = &m.prop else {
                    return self
                        .error(call.span, "library method names must be static identifiers");
                };
                match self.libraries[library.sym.as_ref()].get(method.sym.as_ref()) {
                    Some(tool) => tool.clone(),
                    None => {
                        return self.error(
                            call.span,
                            &format!(
                                "unknown library method {}.{}; use the published library catalog",
                                library.sym, method.sym
                            ),
                        );
                    }
                }
            }
            _ => {
                if let ast::Expr::Ident(name) = target {
                    let alternative = match name.sym.as_ref() {
                        "print" | "println" => "请改用 console.log(...) 输出；其他日志方法见 read_program_guide",
                        _ => "请使用已声明的函数或程序指南中列出的内置函数",
                    };
                    return self.error(call.span, &format!("{} 在流程运行时里不可用；{alternative}", name.sym));
                }
                return self.error(
                    call.span,
                    "use helper(args) or library.method(args); call/ai wrappers are unsupported",
                );
            }
        };
        if args.len() > 1 {
            return self.error(call.span, "a tool call takes one JSON argument object");
        }
        Ok(Effect::Tool(target, args))
    }
    // ------------------------------------------------------------ globals

    /// A global name, unless a local, function, import or library shadows it.
    fn global(&self, name: &str) -> bool {
        self.resolve(name).is_none()
            && !self.libraries.contains_key(name)
            && !self.imports.contains_key(name)
            && !self.names.contains(name)
    }
    /// The variable `process.env.NAME` or `process.env["NAME"]` reads, when
    /// `process` is not one the source binds itself. The host's `process`
    /// library has no `env` method, so Node's spelling is never ambiguous.
    fn environment_variable(&self, m: &ast::MemberExpr) -> Option<String> {
        let ast::Expr::Member(inner) = &*m.obj else { return None };
        let (ast::Expr::Ident(process), ast::MemberProp::Ident(env)) = (&*inner.obj, &inner.prop) else { return None };
        if process.sym != *"process" || env.sym != *"env" || self.resolve("process").is_some()
            || self.imports.contains_key("process") || self.names.contains("process")
        {
            return None;
        }
        match &m.prop {
            ast::MemberProp::Ident(name) => Some(name.sym.to_string()),
            ast::MemberProp::Computed(c) => match &*c.expr {
                ast::Expr::Lit(ast::Lit::Str(name)) => Some(name.value.to_string_lossy().into_owned()),
                _ => None,
            },
            _ => None,
        }
    }
    /// Math.PI, Number.MAX_SAFE_INTEGER and the other static constants.
    fn constant_member(&self, m: &ast::MemberExpr) -> Option<Expr> {
        let ast::Expr::Ident(namespace) = &*m.obj else {
            return None;
        };
        let ast::MemberProp::Ident(property) = &m.prop else {
            return None;
        };
        if !matches!(namespace.sym.as_ref(), "Math" | "Number") || !self.global(namespace.sym.as_ref()) {
            return None;
        }
        super::js_methods::constant(&format!("{}.{}", namespace.sym, property.sym)).map(number_expr)
    }
    /// Global functions, static methods of Math/Number/Object/Array/JSON and
    /// Error constructors called without `new`.
    fn builtin_call(&mut self, call: &ast::CallExpr) -> Result<Option<Expr>> {
        let ast::Callee::Expr(callee) = &call.callee else {
            return Ok(None);
        };
        if matches!(&**callee, ast::Expr::Ident(i) if i.sym == *"$$apply") {
            ensure!(call.args.len() == 2 && call.args.iter().all(|arg| arg.spread.is_none()), "internal apply requires a function and argument array");
            let callee = self.lower(&call.args[0].expr)?;
            let callee = self.snapshot(call.span, callee);
            let args = self.lower(&call.args[1].expr)?;
            let dest = self.temp();
            self.emit(call.span, Op::Effect(Some(dest.clone()), Effect::Apply { callee, args }));
            return Ok(Some(Expr::Variable(dest)));
        }
        let name = match &**callee {
            ast::Expr::Ident(i) if self.global(i.sym.as_ref()) => {
                let name = i.sym.to_string();
                if super::vm_eval::ERROR_CLASSES.contains(&name.as_str()) {
                    format!("new {name}")
                } else {
                    name
                }
            }
            ast::Expr::Member(m) => {
                let (ast::Expr::Ident(namespace), ast::MemberProp::Ident(method)) = (&*m.obj, &m.prop)
                else {
                    return Ok(None);
                };
                if !matches!(namespace.sym.as_ref(), "Math" | "Number" | "String" | "Date" | "Object" | "Array" | "JSON" | "Reflect" | "Symbol")
                    || !self.global(namespace.sym.as_ref())
                {
                    return Ok(None);
                }
                format!("{}.{}", namespace.sym, method.sym)
            }
            _ => return Ok(None),
        };
        let Some((min, max)) = super::js_methods::builtin_arity(&name) else {
            return match &**callee {
                ast::Expr::Member(_) => self.error(call.span, &format!("unsupported built-in {name}. {}", crate::agent_program::NOT_AVAILABLE_HINT)),
                _ => Ok(None),
            };
        };
        if name=="Function" {return self.error(call.span,"Function constructor / dynamic code is unavailable in the flow runtime");}
        if let Some((namespace,method))=name.split_once('.') {
            if call.args.iter().any(|a|a.spread.is_some()) || (name=="Array.from" && (2..=3).contains(&call.args.len())) || (min..=max).contains(&call.args.len()) {
                return self.call_reference(Expr::Intrinsic(namespace.into()),Expr::Value(json!(method)),&call.args,call.span,false).map(Some);
            }
        }
        if call.args.iter().any(|a| a.spread.is_some()) {
            return self.error(call.span, "built-in argument spreads are unsupported");
        }
        if name == "Array.from" && (2..=3).contains(&call.args.len()) {
            let mut args=vec![];
            for arg in &call.args {let value=self.lower(&arg.expr)?;args.push(self.snapshot(arg.span(),value));}
            let destination=self.temp();
            self.emit(call.span,Op::Effect(Some(destination.clone()),Effect::Call("$$arrayFrom".into(),args)));
            return Ok(Some(Expr::Variable(destination)));
        }
        if !(min..=max).contains(&call.args.len()) {
            return self.error(call.span, &format!("{name} takes {min} to {max} arguments"));
        }
        let mut args = Vec::with_capacity(call.args.len());
        for arg in &call.args {
            let value = self.lower(&arg.expr)?;
            args.push(self.snapshot(arg.span(), value));
        }
        Ok(Some(self.snapshot(call.span, Expr::NodeBuiltin(name, args))))
    }

    // ------------------------------------------------------- optional chains

    /// Lower a member object or method receiver. Inside an optional chain it
    /// continues that chain; anywhere else it is an ordinary expression.
    fn lower_object(&mut self, e: &ast::Expr) -> Result<Expr> {
        self.chain_object = true;
        self.lower(e)
    }
    /// Lower an expression. Arguments, keys and operands inside an optional
    /// chain are not part of it: their own chains short-circuit on their own.
    fn lower(&mut self, e: &ast::Expr) -> Result<Expr> {
        if let ast::Expr::Yield(node) = e { return self.lower_yield(node); }
        if let ast::Expr::Call(call) = e
            && matches!(&call.callee, ast::Callee::Expr(c) if matches!(&**c, ast::Expr::Ident(i) if i.sym == *"$$generatorYieldResult"))
        {
            ensure!(call.args.len() == 1, "invalid internal generator yield");
            let value = self.lower(&call.args[0].expr)?;
            return self.generator_yield(value, call.span, true);
        }
        let joins_chain = std::mem::take(&mut self.chain_object);
        if !joins_chain && self.chain_exits.is_some() {
            let saved = self.chain_exits.take();
            let result = self.lower_inner(e);
            self.chain_exits = saved;
            return result;
        }
        self.lower_inner(e)
    }
    /// One optional chain: a `?.` whose left side is null or undefined makes
    /// the whole chain undefined without evaluating the rest of it.
    fn with_chain(&mut self, span: Span, f: impl FnOnce(&mut Self) -> Result<Expr>) -> Result<Expr> {
        if self.chain_exits.is_some() {
            return f(self);
        }
        self.chain_exits = Some(Vec::new());
        let value = f(self);
        let exits = self.chain_exits.take().unwrap_or_default();
        let value = value?;
        if exits.is_empty() {
            return Ok(value);
        }
        let result = self.temp();
        self.emit(span, Op::Set(result.clone(), value));
        let jump = self.emit(span, Op::Jump(0));
        let exit = self.code.len();
        self.emit(span, Op::Set(result.clone(), Expr::Undefined));
        let end = self.code.len();
        self.code[jump].op = Op::Jump(end);
        for at in exits {
            if let Op::Branch(_, target) = &mut self.code[at].op {
                *target = exit;
            }
        }
        Ok(Expr::Variable(result))
    }
    fn chain_guard(&mut self, span: Span, value: Expr) {
        let test = Expr::Binary("!=".into(), Box::new(value), Box::new(Expr::Value(Value::Null)));
        let at = self.emit(span, Op::Branch(test, 0));
        self.chain_exits
            .as_mut()
            .expect("a chain guard is emitted inside an optional chain")
            .push(at);
    }
    fn member_key(&mut self, prop: &ast::MemberProp, _span: Span) -> Result<Expr> {
        Ok(match prop {
            ast::MemberProp::Ident(i) => Expr::Value(json!(i.sym.to_string())),
            ast::MemberProp::PrivateName(i) => self.private_key(i)?,
            ast::MemberProp::Computed(c) => {
                let key = self.lower(&c.expr)?;
                self.snapshot(c.span, Expr::NodeBuiltin("$$propertyKey".into(),vec![key]))
            }
        })
    }
    fn chain_link(&mut self, chain: &ast::OptChainExpr) -> Result<Expr> {
        match &*chain.base {
            ast::OptChainBase::Member(m) => {
                let object = self.lower_object(&m.obj)?;
                let object = self.snapshot(m.obj.span(), object);
                if chain.optional {
                    self.chain_guard(chain.span, object.clone());
                }
                let key = self.member_key(&m.prop, m.span)?;
                Ok(Expr::Member(Box::new(object), Box::new(key)))
            }
            ast::OptChainBase::Call(call) => {
                let mut target=&*call.callee;
                while let ast::Expr::Paren(p)=target {target=&p.expr;}
                let member=match target {
                    ast::Expr::Member(m)=>Some((m,false)),
                    ast::Expr::OptChain(inner)=>match &*inner.base {ast::OptChainBase::Member(m)=>Some((m,inner.optional)),_=>None},
                    _=>None,
                };
                if let Some((member,optional))=member {
                    if let Some(callee)=self.native_value(target)? {
                        if chain.optional {self.chain_guard(call.span,callee.clone());}
                        return self.invoke_reference(callee,Expr::Undefined,&call.args,call.span,None);
                    }
                    let receiver=self.lower_object(&member.obj)?;
                    let receiver=self.snapshot(member.obj.span(),receiver);
                    if optional {self.chain_guard(call.span,receiver.clone());}
                    let key=self.member_key(&member.prop,member.span)?;
                    self.call_reference(receiver,key,&call.args,call.span,chain.optional)
                } else {
                    let value=self.lower_object(&call.callee)?;
                    let callee=self.snapshot(call.callee.span(),value);
                    if chain.optional {self.chain_guard(call.span,callee.clone());}
                    self.invoke_reference(callee,Expr::Undefined,&call.args,call.span,None)
                }
            }
        }
    }

    // --------------------------------------------------------------- methods

    fn call_method_spread(&mut self, receiver: Expr, key: Expr, args: &[ast::ExprOrSpread], span: Span) -> Result<Expr> {
        self.call_reference(receiver,key,args,span,false)
    }
    fn call_reference(&mut self, receiver: Expr, key: Expr, args: &[ast::ExprOrSpread], span: Span, optional: bool) -> Result<Expr> {
        let callee=self.snapshot(span,Expr::Member(Box::new(receiver.clone()),Box::new(key.clone())));
        if optional {self.chain_guard(span,callee.clone());}
        self.invoke_reference(callee,receiver,args,span,Some(key))
    }
    fn invoke_reference(&mut self, callee: Expr, receiver: Expr, args: &[ast::ExprOrSpread], span: Span, label: Option<Expr>) -> Result<Expr> {
        let array = ast::Expr::Array(ast::ArrayLit { span, elems: args.iter().cloned().map(Some).collect() });
        let args = self.lower(&array)?;
        let dest = self.temp();
        self.emit(span,Op::Effect(Some(dest.clone()),Effect::Invoke(Box::new(Invocation{callee,receiver,args,label}))));
        Ok(Expr::Variable(dest))
    }

    /// Lower a tagged template into the ordinary call protocol. The first
    /// argument is the cooked string array with its raw view attached. A
    /// member tag keeps its receiver exactly like `obj.tag(...)`.
    fn lower_tagged_template(&mut self, tagged: &ast::TaggedTpl) -> Result<Expr> {
        let cooked = tagged
            .tpl
            .quasis
            .iter()
            .map(|quasi| {
                let value = quasi.cooked.as_ref().and_then(|value| value.as_str()).unwrap_or("");
                Expr::Value(json!(value))
            })
            .collect::<Vec<_>>();
        let raw = tagged
            .tpl
            .quasis
            .iter()
            .map(|quasi| Expr::Value(json!(quasi.raw.to_string())))
            .collect::<Vec<_>>();
        let strings = self.temp();
        self.emit(tagged.span, Op::Set(strings.clone(), Expr::Array(cooked)));
        self.emit(
            tagged.span,
            Op::SetMember {
                object: Expr::Variable(strings.clone()),
                key: Expr::Value(json!("raw")),
                value: Expr::Array(raw),
            },
        );
        let mut values = vec![Expr::Variable(strings)];
        for expression in &tagged.tpl.exprs {
            let value = self.lower(expression)?;
            values.push(self.snapshot(expression.span(), value));
        }
        let args = Expr::Array(values);
        let destination = self.temp();
        let tag = &*tagged.tag;
        if let ast::Expr::Member(member) = tag {
            let receiver_value = self.lower_object(&member.obj)?;
            let receiver = self.snapshot(member.obj.span(), receiver_value);
            let key = self.member_key(&member.prop, member.span)?;
            let callee = self.snapshot(tagged.span, Expr::Member(Box::new(receiver.clone()), Box::new(key.clone())));
            self.emit(
                tagged.span,
                Op::Effect(
                    Some(destination.clone()),
                    Effect::Invoke(Box::new(Invocation {
                        callee,
                        receiver,
                        args,
                        label: Some(key),
                    })),
                ),
            );
        } else {
            let callee_value = self.lower(tag)?;
            let callee = self.snapshot(tagged.tag.span(), callee_value);
            self.emit(
                tagged.span,
                Op::Effect(Some(destination.clone()), Effect::Dynamic { callee, args: match args {
                    Expr::Array(values) => values,
                    _ => unreachable!(),
                }}),
            );
        }
        Ok(Expr::Variable(destination))
    }

    /// `receiver.method(args)` for a built-in receiver kind. Callback methods
    /// are compiled into this frame so that their callbacks can suspend.
    fn call_method_on(
        &mut self,
        receiver: Expr,
        method: &str,
        args: &[ast::ExprOrSpread],
        span: Span,
    ) -> Result<Expr> {
        if args.iter().any(|a| a.spread.is_some()) {
            return self.call_method_spread(receiver, Expr::Value(json!(method)), args, span);
        }
        if let Some(reason) = super::js_methods::unavailable_method(method) {
            return self.error(span, reason);
        }
        // Resolve before evaluating arguments. An own function-valued property
        // need not have the built-in method's parameter count.
        self.call_reference(receiver,Expr::Value(json!(method)),args,span,false)
    }

    fn land(&mut self, at: usize) {
        let here = self.code.len();
        match &mut self.code[at].op {
            Op::Branch(_, target) | Op::Jump(target) => *target = here,
            _ => unreachable!("only branches and jumps are patched"),
        }
    }

    fn lower_construct(&mut self, new: &ast::NewExpr) -> Result<Expr> {
        let value = self.lower(&new.callee)?;
        let callee = self.snapshot(new.callee.span(), value);
        let array = ast::Expr::Array(ast::ArrayLit { span: new.span, elems: new.args.as_deref().unwrap_or(&[]).iter().cloned().map(Some).collect() });
        let args = self.lower(&array)?;
        let dest = self.temp();
        self.emit(new.span, Op::Effect(Some(dest.clone()), Effect::Construct(Box::new(Invocation { callee, receiver: Expr::Undefined, args, label: None }))));
        Ok(Expr::Variable(dest))
    }

    /// Binary chains are common in generated HTML. Keep the traversal on a
    /// work stack: lower_inner has a large frame, so recursively lowering even
    /// a few dozen concatenations can exhaust a runtime worker's native stack.
    fn lower_binary(&mut self, root: &ast::BinExpr) -> Result<Expr> {
        fn eager(binary: &ast::BinExpr) -> bool {
            !matches!(binary.op, ast::BinaryOp::LogicalAnd | ast::BinaryOp::LogicalOr | ast::BinaryOp::NullishCoalescing)
                && !(binary.op == ast::BinaryOp::In && matches!(&*binary.left, ast::Expr::PrivateName(_)))
        }
        enum Work<'a> {
            Lower(&'a ast::Expr),
            Right(&'a ast::BinExpr),
            Finish(&'a ast::BinExpr, Expr),
        }
        let mut work = vec![Work::Right(root), Work::Lower(&root.left)];
        let mut value = None;
        while let Some(next) = work.pop() {
            match next {
                Work::Lower(expr) => {
                    if let ast::Expr::Bin(binary) = expr && eager(binary) {
                        self.reject_unawaited_operands(expr)?;
                        work.push(Work::Right(binary));
                        work.push(Work::Lower(&binary.left));
                    } else {
                        value = Some(self.lower(expr)?);
                    }
                }
                Work::Right(binary) => {
                    let left = self.snapshot(binary.left.span(), value.take().expect("binary left operand"));
                    work.push(Work::Finish(binary, left));
                    work.push(Work::Lower(&binary.right));
                }
                Work::Finish(binary, left) => {
                    let right = self.snapshot(binary.right.span(), value.take().expect("binary right operand"));
                    let op = match binary.op {
                        ast::BinaryOp::EqEq => "==",
                        ast::BinaryOp::NotEq => "!=",
                        ast::BinaryOp::EqEqEq => "===",
                        ast::BinaryOp::NotEqEq => "!==",
                        ast::BinaryOp::Lt => "<",
                        ast::BinaryOp::LtEq => "<=",
                        ast::BinaryOp::Gt => ">",
                        ast::BinaryOp::GtEq => ">=",
                        ast::BinaryOp::Add => "+",
                        ast::BinaryOp::Sub => "-",
                        ast::BinaryOp::Mul => "*",
                        ast::BinaryOp::Div => "/",
                        ast::BinaryOp::Mod => "%",
                        ast::BinaryOp::Exp => "**",
                        ast::BinaryOp::BitAnd => "&",
                        ast::BinaryOp::BitOr => "|",
                        ast::BinaryOp::BitXor => "^",
                        ast::BinaryOp::LShift => "<<",
                        ast::BinaryOp::RShift => ">>",
                        ast::BinaryOp::ZeroFillRShift => ">>>",
                        ast::BinaryOp::In => "in",
                        ast::BinaryOp::InstanceOf => "instanceof",
                        _ => unreachable!("short-circuit operators use logical lowering"),
                    };
                    value = Some(Expr::Binary(op.into(), Box::new(left), Box::new(right)));
                }
            }
        }
        Ok(value.expect("binary expression result"))
    }

    fn lower_inner(&mut self, e: &ast::Expr) -> Result<Expr> {
        self.reject_unawaited_operands(e)?;
        match e {
            ast::Expr::Seq(seq) => {
                let Some((last, preceding)) = seq.exprs.split_last() else { return Ok(Expr::Undefined); };
                for expr in preceding { self.discard(expr)?; }
                self.lower(last)
            }
            // A TextEncoder is stateless: an object whose encode is the
            // runtime's own UTF-8 encoder, as require("path") gives path.
            ast::Expr::New(new) if matches!(&*new.callee, ast::Expr::Ident(i) if i.sym == *"TextEncoder") && self.global("TextEncoder") => {
                Ok(Expr::Object(vec![
                    ("encoding".into(), Expr::Value(json!("utf-8"))),
                    ("encode".into(), Expr::Native("TextEncoder.encode".into())),
                ]))
            }
            // Spread constructors use the same durable invocation as aliases.
            ast::Expr::New(new) if new.args.as_deref().unwrap_or(&[]).iter().any(|a| a.spread.is_some()) => {
                self.lower_construct(new)
            }
            ast::Expr::New(new) if matches!(&*new.callee, ast::Expr::Ident(i) if matches!(i.sym.as_ref(), "Array" | "Date" | "Map" | "Set" | "WeakMap" | "WeakSet") && self.global(i.sym.as_ref())) => {
                let ast::Expr::Ident(name) = &*new.callee else { unreachable!() };
                let args = new.args.as_deref().unwrap_or(&[]);
                if args.iter().any(|arg| arg.spread.is_some()) { return self.error(new.span, "constructor argument spreads are unsupported; use Array.from for iterable values"); }
                let mut values = Vec::new();
                for arg in args {
                    let value = self.lower(&arg.expr)?;
                    values.push(self.snapshot(arg.span(), value));
                }
                Ok(self.snapshot(new.span, Expr::NodeBuiltin(format!("new {}", name.sym), values)))
            }
            ast::Expr::TaggedTpl(tagged) => self.lower_tagged_template(tagged),
            ast::Expr::OptChain(chain) => self.with_chain(chain.span, |c| c.chain_link(chain)),
            ast::Expr::New(new) if cfg!(test) && self.external.contains("__test262CheckThrown") && matches!(&*new.callee,ast::Expr::Ident(i) if i.sym==*"Test262Error") => {
                let mut args=vec![];
                for arg in new.args.as_deref().unwrap_or(&[]) {args.push(self.lower(&arg.expr)?);}
                Ok(self.snapshot(new.span,Expr::NodeBuiltin("test262.error".into(),args)))
            }
            ast::Expr::New(new)
                if matches!(&*new.callee, ast::Expr::Ident(i)
                    if super::vm_eval::ERROR_CLASSES.contains(&i.sym.as_ref()) && self.global(i.sym.as_ref())) =>
            {
                let ast::Expr::Ident(class) = &*new.callee else { unreachable!() };
                let args = new.args.as_deref().unwrap_or(&[]);
                if args.len() > 2 || args.iter().any(|a| a.spread.is_some()) {
                    return self.error(new.span, "Error constructors take a message and an optional options object");
                }
                let mut lowered = Vec::with_capacity(args.len());
                for arg in args {
                    let value = self.lower(&arg.expr)?;
                    lowered.push(self.snapshot(arg.span(), value));
                }
                Ok(self.snapshot(new.span, Expr::NodeBuiltin(format!("new {}", class.sym), lowered)))
            }
            ast::Expr::New(new) if !matches!(&*new.callee,ast::Expr::Ident(i) if self.resolve(i.sym.as_ref()).is_none() && !self.names.contains(i.sym.as_ref()) && !matches!(i.sym.as_ref(),"Object"|"Number"|"String"|"Boolean"|"Symbol")) => {
                self.lower_construct(new)
            }
            ast::Expr::Unary(u) if u.op == ast::UnaryOp::Delete => {
                let ast::Expr::Member(m) = &*u.arg else {
                    return self.error(u.span, "delete needs a property access such as delete object.key");
                };
                let object = self.lower_object(&m.obj)?;
                let object = self.snapshot(m.obj.span(), object);
                let key = self.member_key(&m.prop, m.span)?;
                Ok(self.snapshot(u.span, Expr::NodeBuiltin("delete".into(), vec![object, key])))
            }
            ast::Expr::Unary(u)
                if u.op == ast::UnaryOp::TypeOf
                    && matches!(&*u.arg, ast::Expr::Ident(i) if self.resolve(i.sym.as_ref()).is_none()) =>
            {
                // typeof never throws for an undeclared name.
                let ast::Expr::Ident(i) = &*u.arg else { unreachable!() };
                let kind = if self.names.contains(i.sym.as_ref()) {
                    "function"
                } else if NATIVE_GLOBALS.contains(&i.sym.as_ref()) || matches!(i.sym.as_ref(), "atob" | "btoa" | "TextEncoder") {
                    "function"
                } else if matches!(i.sym.as_ref(), "Math" | "JSON" | "Reflect" | "Object" | "path" | "url" | "os") {
                    "object"
                } else if i.sym == *"undefined" {
                    "undefined"
                } else if i.sym == *"NaN" || i.sym == *"Infinity" {
                    "number"
                } else {
                    "undefined"
                };
                Ok(Expr::Value(json!(kind)))
            }
            ast::Expr::New(new) if matches!(&*new.callee, ast::Expr::Ident(i) if i.sym == *"Promise") && self.global("Promise") => {
                let args = new.args.as_deref().unwrap_or(&[]);
                if args.iter().any(|arg| arg.spread.is_some()) {
                    return self.error(new.span, "Promise constructor spread arguments are not yet supported");
                }
                let mut values = Vec::new();
                for arg in args {
                    let value = self.lower(&arg.expr)?;
                    values.push(self.snapshot(arg.span(), value));
                }
                let executor = values.into_iter().next().unwrap_or(Expr::Undefined);
                let destination = self.temp();
                self.emit(new.span, Op::Effect(Some(destination.clone()), Effect::PromiseConstruct { executor }));
                Ok(Expr::Variable(destination))
            }
            ast::Expr::New(new) if matches!(&*new.callee, ast::Expr::Ident(i) if i.sym == *"RegExp") => {
                if self.resolve("RegExp").is_some() {
                    return self.error(new.span, "shadowed RegExp constructor is unsupported");
                }
                let args = new.args.as_deref().unwrap_or(&[]);
                if args.len() > 2 || args.iter().any(|a| a.spread.is_some()) {
                    return self.error(new.span, "RegExp accepts at most two arguments without spread");
                }
                let mut lowered = Vec::new();
                for arg in args {
                    let value = self.lower(&arg.expr)?;
                    lowered.push(self.snapshot(arg.span(), value));
                }
                Ok(Expr::RegExp(lowered))
            }
            ast::Expr::Await(awaited) => {
                let value = self.lower(&awaited.arg)?;
                Ok(self.await_value(value,awaited.span))
            }
            ast::Expr::Bin(b) if b.op == ast::BinaryOp::In && matches!(&*b.left, ast::Expr::PrivateName(_)) => {
                let ast::Expr::PrivateName(name) = &*b.left else { unreachable!() };
                let key = self.private_key(name)?;
                let object = self.lower(&b.right)?;
                Ok(self.snapshot(b.span, Expr::NodeBuiltin("$$privateHas".into(), vec![object,key])))
            }
            ast::Expr::Bin(b) if matches!(b.op, ast::BinaryOp::LogicalAnd | ast::BinaryOp::LogicalOr | ast::BinaryOp::NullishCoalescing) => {
                let op = match b.op {
                    ast::BinaryOp::LogicalAnd => "&&",
                    ast::BinaryOp::LogicalOr => "||",
                    ast::BinaryOp::NullishCoalescing => "??",
                    _ => unreachable!(),
                };
                let left = self.lower(&b.left)?;
                self.logical(op, left, &b.right, b.span)
            }
            ast::Expr::Bin(b) => self.lower_binary(b),
            ast::Expr::Unary(u) => {
                let op = match u.op {
                    ast::UnaryOp::Bang => "!",
                    ast::UnaryOp::Plus => "+",
                    ast::UnaryOp::Minus => "-",
                    ast::UnaryOp::TypeOf => "typeof",
                    ast::UnaryOp::Tilde => "~",
                    ast::UnaryOp::Void => "void",
                    _ => return self.error(u.span, "unsupported unary operator"),
                };
                let value = self.lower(&u.arg)?;
                Ok(Expr::Unary(op.into(), Box::new(value)))
            }
            ast::Expr::Cond(c) => {
                let test = self.condition(&c.test)?;
                let branch = self.emit(c.span, Op::Branch(test, 0));
                let result = self.temp();
                self.value(&c.cons, result.clone())?;
                let jump = self.emit(c.span, Op::Jump(0));
                let alternative = self.code.len();
                self.value(&c.alt, result.clone())?;
                let end = self.code.len();
                if let Op::Branch(_, target) = &mut self.code[branch].op {
                    *target = alternative;
                }
                self.code[jump].op = Op::Jump(end);
                Ok(Expr::Variable(result))
            }
            ast::Expr::Tpl(t) => {
                let mut parts = Vec::new();
                for (i, quasi) in t.quasis.iter().enumerate() {
                    let cooked = quasi
                        .cooked
                        .as_ref()
                        .context("invalid template escape")?;
                    parts.push(match cooked.as_str() {
                        Some(text) => Expr::Value(json!(text)),
                        None => Expr::Utf16(cooked.as_wtf8().to_ill_formed_utf16().collect()),
                    });
                    if let Some(e) = t.exprs.get(i) {
                        let value = self.lower(e)?;
                        // Convert now, before any later interpolation has effects.
                        let value = Expr::Template(vec![value]);
                        parts.push(self.snapshot(e.span(), value));
                    }
                }
                Ok(Expr::Template(parts))
            }
            ast::Expr::Assign(a) => {
                if let ast::AssignTarget::Pat(pat) = &a.left {
                    let pat: ast::Pat = pat.clone().into();
                    let value = self.lower(&a.right)?;
                    let value = self.snapshot(a.right.span(), value);
                    for name in pattern_names(&pat) { self.local_target(&name, a.span)?; }
                    self.bind_value(&pat, value.clone(), false)?;
                    return Ok(value);
                }
                if let ast::AssignTarget::Simple(ast::SimpleAssignTarget::SuperProp(p))=&a.left{return self.super_assignment(a,p);}
                if let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) = &a.left {
                    let object = self.lower(&member.obj)?;
                    let object = self.snapshot(member.obj.span(), object);
                    let key = match &member.prop {
                        ast::MemberProp::Ident(name) => Expr::Value(json!(name.sym.to_string())),
                        ast::MemberProp::PrivateName(name) => self.private_key(name)?,
                        ast::MemberProp::Computed(key) => { let value = self.lower(&key.expr)?; self.snapshot(key.span, Expr::NodeBuiltin("$$propertyKey".into(),vec![value])) },
                    };
                    if matches!(a.op, ast::AssignOp::AndAssign | ast::AssignOp::OrAssign | ast::AssignOp::NullishAssign) {
                        let old = self.snapshot(a.span, Expr::Member(Box::new(object.clone()), Box::new(key.clone())));
                        let result = self.temp();
                        self.emit(a.span, Op::Set(result.clone(), old.clone()));
                        let skip = self.emit(a.span, Op::Branch(Self::logical_assignment_test(a.op, old), 0));
                        let value = self.lower(&a.right)?;
                        let value = self.snapshot(a.right.span(), value);
                        self.emit(a.span, Op::SetMember { object, key, value: value.clone() });
                        self.emit(a.span, Op::Set(result.clone(), value));
                        self.land(skip);
                        return Ok(Expr::Variable(result));
                    }
                    let op = Self::compound_operator(a.op);
                    if a.op != ast::AssignOp::Assign && op.is_none() {
                        return self.error(a.span, "unsupported compound member assignment");
                    }
                    let old = op.map(|_| self.snapshot(a.span, Expr::Member(Box::new(object.clone()), Box::new(key.clone()))));
                    let value = self.lower(&a.right)?;
                    let value = if let (Some(op), Some(old)) = (op, old) { Expr::Binary(op.into(), Box::new(old), Box::new(value)) } else { value };
                    let value = self.snapshot(a.right.span(), value);
                    self.emit(a.span, Op::SetMember { object, key, value: value.clone() });
                    return Ok(value);
                }
                let ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(i)) = &a.left else {
                    return self.error(a.span, "unsupported assignment target");
                };
                let name = i.id.sym.to_string();
                self.local_target(&name, a.span)?;
                if matches!(a.op, ast::AssignOp::AndAssign | ast::AssignOp::OrAssign | ast::AssignOp::NullishAssign) {
                    let old = self.snapshot(a.span, Expr::Variable(name.clone()));
                    let skip = self.emit(a.span, Op::Branch(Self::logical_assignment_test(a.op, old), 0));
                    let value = self.lower_named(&a.right,Some(&name))?;
                    self.emit(a.span, Op::Set(name.clone(), value));
                    self.land(skip);
                    return Ok(Expr::Variable(name));
                }
                let op = Self::compound_operator(a.op);
                if a.op != ast::AssignOp::Assign && op.is_none() {
                    return self.error(a.span, "unsupported compound assignment");
                }
                let old = op.map(|_| self.snapshot(a.span, Expr::Variable(name.clone())));
                let value = if a.op==ast::AssignOp::Assign{self.lower_named(&a.right,Some(&name))?}else{self.lower(&a.right)?};
                let value = if let (Some(op), Some(old)) = (op, old) {
                    Expr::Binary(op.into(), Box::new(old), Box::new(value))
                } else {
                    value
                };
                self.emit(a.span, Op::Set(name.clone(), value));
                Ok(Expr::Variable(name))
            }
            ast::Expr::Update(u) => {
                if let ast::Expr::SuperProp(p)=&*u.arg{return self.super_update(u,p);}
                if let ast::Expr::Member(member) = &*u.arg {
                    let object = self.lower(&member.obj)?;
                    let object = self.snapshot(member.obj.span(), object);
                    let key = match &member.prop {
                        ast::MemberProp::Ident(name) => Expr::Value(json!(name.sym.to_string())),
                        ast::MemberProp::PrivateName(name) => self.private_key(name)?,
                        ast::MemberProp::Computed(key) => { let value = self.lower(&key.expr)?; self.snapshot(key.span, Expr::NodeBuiltin("$$propertyKey".into(),vec![value])) },
                    };
                    let old = self.snapshot(u.span, Expr::Unary("+".into(), Box::new(Expr::Member(Box::new(object.clone()), Box::new(key.clone())))));
                    let op = if u.op == ast::UpdateOp::PlusPlus { "+" } else { "-" };
                    let value = self.snapshot(u.span, Expr::Binary(op.into(), Box::new(old.clone()), Box::new(Expr::Value(json!(1)))));
                    self.emit(u.span, Op::SetMember { object, key, value: value.clone() });
                    return Ok(if u.prefix { value } else { old });
                }
                let ast::Expr::Ident(i) = &*u.arg else {
                    return self.error(u.span, "update requires a local variable");
                };
                let name = i.sym.to_string();
                self.local_target(&name, u.span)?;
                let old = self.snapshot(
                    u.span,
                    Expr::Unary("+".into(), Box::new(Expr::Variable(name.clone()))),
                );
                let op = if u.op == ast::UpdateOp::PlusPlus {
                    "+"
                } else {
                    "-"
                };
                self.emit(
                    u.span,
                    Op::Set(
                        name.clone(),
                        Expr::Binary(
                            op.into(),
                            Box::new(old.clone()),
                            Box::new(Expr::Value(json!(1))),
                        ),
                    ),
                );
                Ok(if u.prefix { Expr::Variable(name) } else { old })
            }
            ast::Expr::Call(call) => {
                if matches!(call.callee,ast::Callee::Super(_)){return self.super_call(call);}
                if let ast::Callee::Expr(e)=&call.callee && let ast::Expr::SuperProp(p)=&**e {let callee=self.super_get(p)?;return self.invoke_reference(callee,Expr::Variable("\0this".into()),&call.args,call.span,None);}
                if matches!(&call.callee,ast::Callee::Expr(e) if matches!(&**e,ast::Expr::Member(m) if matches!(&*m.obj,ast::Expr::Ident(i) if i.sym==*"tools")))
                    && self.global("tools")
                {
                    return self.error(call.span,"tools.* is unsupported; use the fixed library.method(args) names in program_tool_headers");
                }
                if matches!(&call.callee,ast::Callee::Expr(e) if matches!(&**e,ast::Expr::Ident(i) if i.sym==*"assert")) && self.global("assert")
                {
                    if !(1..=2).contains(&call.args.len())
                        || call.args.iter().any(|a| a.spread.is_some())
                    {
                        return self.error(
                            call.span,
                            "assert takes a condition and optional failure message",
                        );
                    }
                    let test = self.condition(&call.args[0].expr)?;
                    let branch = self.emit(call.span, Op::Branch(test, 0));
                    let success = self.emit(call.span, Op::Jump(0));
                    let failure = self.code.len();
                    let error = if let Some(message) = call.args.get(1) {
                        self.lower(&message.expr)?
                    } else {
                        Expr::Value(json!({"message":"assertion failed"}))
                    };
                    self.emit(call.span, Op::Throw(error));
                    let end = self.code.len();
                    self.code[branch].op = match &self.code[branch].op {
                        Op::Branch(test, _) => Op::Branch(test.clone(), failure),
                        _ => unreachable!(),
                    };
                    self.code[success].op = Op::Jump(end);
                    return Ok(Expr::Value(json!(true)));
                }
                // Both host SDK and third-party libraries load at this line.
                if let ast::Callee::Expr(target) = &call.callee
                    && matches!(&**target, ast::Expr::Ident(i) if i.sym == *"require")
                    && self.global("require")
                {
                    let Some(library) = required_name(call) else {
                        return self.error(call.span, "require takes one library name string, such as require(\"ppt\")");
                    };
                    if let Some((module, functions)) = node_builtin_module(&library)
                        && !self.libraries.contains_key(module)
                    {
                        return Ok(Expr::Object(functions.iter().map(|function| ((*function).to_owned(), Expr::Native(format!("{module}.{function}")))).collect()));
                    }
                    let Some(specifier) = library_specifier(&library) else {
                        return self.error(
                            call.span,
                            &format!("require(\"{library}\"): a library name has letters, digits, _ or -, optionally followed by /promises; host: is an optional compatibility prefix"),
                        );
                    };
                    let destination = self.temp();
                    self.emit(call.span, Op::Effect(Some(destination.clone()), Effect::Require(specifier.to_owned())));
                    return Ok(Expr::Variable(destination));
                }
                if call.args.iter().any(|arg| arg.spread.is_some()) {
                    let ast::Callee::Expr(target) = &call.callee else { return self.error(call.span, "spread call needs a function value"); };
                    let mut target=&**target;
                    while let ast::Expr::Paren(p)=target {target=&p.expr;}
                    if matches!(target,ast::Expr::OptChain(chain) if matches!(&*chain.base,ast::OptChainBase::Member(_))) {return self.dynamic_call(call,target);}
                    if matches!(target, ast::Expr::Ident(i) if i.sym == *"assert") && self.global("assert") {
                        return self.error(call.span, "spread is unavailable for this special call");
                    }
                    if let ast::Expr::Member(member) = target && self.native_value(target)?.is_none() {
                        let receiver = self.lower(&member.obj)?;
                        let receiver = self.snapshot(member.obj.span(), receiver);
                        let key = self.member_key(&member.prop, member.span)?;
                        return self.call_method_spread(receiver, key, &call.args, call.span);
                    }
                    let callee = self.lower(target)?;
                    let callee = self.snapshot(target.span(), callee);
                    let array = ast::Expr::Array(ast::ArrayLit { span: call.span, elems: call.args.iter().cloned().map(Some).collect() });
                    let args = self.lower(&array)?;
                    let dest = self.temp();
                    self.emit(call.span, Op::Effect(Some(dest.clone()), Effect::Apply { callee, args }));
                    return Ok(Expr::Variable(dest));
                }
                // Console and scheduling mutate durable Runtime state.
                if let ast::Callee::Expr(target) = &call.callee
                    && let Some(callee) = self.native_value(target)?
                    && matches!(&callee, Expr::Native(name) if name.starts_with("console.") || matches!(name.as_str(), "setTimeout" | "setInterval" | "clearTimeout" | "clearInterval" | "queueMicrotask"))
                {
                    let mut args = Vec::new();
                    for arg in &call.args {
                        let value = self.lower(&arg.expr)?;
                        args.push(self.snapshot(arg.span(), value));
                    }
                    let dest = self.temp();
                    self.emit(call.span, Op::Effect(Some(dest.clone()), Effect::Dynamic { callee, args }));
                    return Ok(Expr::Variable(dest));
                }
                if let ast::Callee::Expr(target) = &call.callee
                    && let ast::Expr::Member(member) = &**target
                    && matches!(&*member.obj, ast::Expr::Ident(i) if i.sym == *"Promise")
                    && self.global("Promise")
                    && let ast::MemberProp::Ident(method) = &member.prop
                    && matches!(method.sym.as_ref(), "resolve" | "reject")
                {
                    return self.call_reference(Expr::Intrinsic("Promise".into()),Expr::Value(json!(method.sym.to_string())),&call.args,call.span,false);
                }
                if let ast::Callee::Expr(target) = &call.callee
                    && let ast::Expr::Member(member) = &**target
                    && matches!(&*member.obj, ast::Expr::Ident(i) if i.sym == *"Promise")
                    && self.global("Promise")
                    && let ast::MemberProp::Ident(method) = &member.prop
                    && matches!(method.sym.as_ref(), "all" | "allSettled" | "race" | "any")
                {
                    return self.call_reference(Expr::Intrinsic("Promise".into()),Expr::Value(json!(method.sym.to_string())),&call.args,call.span,false);
                }
                if let Some(value) = self.builtin_call(call)? {
                    return Ok(value);
                }
                if let ast::Callee::Expr(target) = &call.callee
                    && let ast::Expr::Ident(function) = &**target
                    && matches!(function.sym.as_ref(), "atob" | "btoa")
                    && self.global(function.sym.as_ref())
                {
                    if call.args.len() != 1 || call.args[0].spread.is_some() {
                        return self.error(call.span, &format!("{} takes one string argument", function.sym));
                    }
                    let value = self.lower(&call.args[0].expr)?;
                    let value = self.snapshot(call.args[0].span(), value);
                    return Ok(Expr::NodeBuiltin(function.sym.to_string(), vec![value]));
                }
                if let ast::Callee::Expr(target) = &call.callee
                    && let ast::Expr::Member(member) = &**target
                    && let ast::Expr::Ident(namespace) = &*member.obj
                    && matches!(namespace.sym.as_ref(), "path" | "url" | "os" | "Buffer")
                    && self.resolve(namespace.sym.as_ref()).is_none()
                    && !self.libraries.contains_key(namespace.sym.as_ref())
                {
                    let ast::MemberProp::Ident(method) = &member.prop else {
                        return self.error(call.span, "built-in method must be a static name");
                    };
                    let name = format!("{}.{}", namespace.sym, method.sym);
                    let (min, max) = super::builtins::arity(&name).ok_or_else(|| {
                        let p = self.at(call.span);
                        anyhow::anyhow!(
                            "TS_PROGRAM {}:{}: unsupported built-in {name}. {}",
                            p.line,
                            p.column,
                            crate::agent_program::NOT_AVAILABLE_HINT
                        )
                    })?;
                    if !(min..=max).contains(&call.args.len())
                        || call.args.iter().any(|arg| arg.spread.is_some())
                    {
                        return self.error(call.span, "invalid built-in arguments");
                    }
                    let mut args = Vec::new();
                    for arg in &call.args {
                        let value = self.lower(&arg.expr)?;
                        args.push(self.snapshot(arg.span(), value));
                    }
                    return Ok(Expr::NodeBuiltin(name, args));
                }
                if matches!(&call.callee,ast::Callee::Expr(e) if matches!(&**e,ast::Expr::Member(m) if !matches!(&*m.obj,ast::Expr::Ident(i) if self.libraries.contains_key(i.sym.as_ref()))))
                {
                    let ast::Callee::Expr(target) = &call.callee else {
                        unreachable!()
                    };
                    let ast::Expr::Member(member) = &**target else {
                        unreachable!()
                    };
                    let ast::MemberProp::Ident(method) = &member.prop else {
                        // obj[key](…) calls the function stored at that key.
                        return self.dynamic_call(call, target);
                    };
                    if matches!(&*member.obj, ast::Expr::Ident(i) if i.sym == *"JSON") {
                        return self.error(call.span, "only JSON.stringify and JSON.parse are available");
                    }
                    let receiver = self.lower_object(&member.obj)?;
                    let receiver = self.snapshot(member.obj.span(), receiver);
                    return self.call_method_on(receiver, method.sym.as_ref(), &call.args, call.span);
                }
                if call.args.iter().any(|a| a.spread.is_some()) {
                    return self.error(call.span, "argument spread is unsupported");
                }
                let mut args = Vec::new();
                let target = match &call.callee {
                    ast::Callee::Expr(e) => &**e,
                    _ => {
                        return self.error(call.span, "use a declared function or library.method");
                    }
                };
                if !self.static_callee(target) {
                    return self.dynamic_call(call, target);
                }
                for arg in &call.args {
                    let lowered = self.lower(&arg.expr)?;
                    let name = self.temp();
                    self.emit(arg.span(), Op::Set(name.clone(), lowered));
                    args.push(ast::ExprOrSpread {
                        spread: None,
                        expr: Box::new(ast::Expr::Ident(ast::Ident::new_no_ctxt(
                            name.into(),
                            arg.span(),
                        ))),
                    });
                    self.temp_names.insert(match &*args.last().unwrap().expr {
                        ast::Expr::Ident(i) => i.sym.to_string(),
                        _ => unreachable!(),
                    });
                }
                let mut translated = call.clone();
                translated.args = args;
                let effect = self.effect(&translated)?;
                let dest = self.temp();
                self.emit(call.span, Op::Effect(Some(dest.clone()), effect));
                Ok(Expr::Variable(dest))
            }
            ast::Expr::Class(c)=>self.lower_class(&c.class,c.ident.as_ref().map(|n|n.sym.to_string())),
            ast::Expr::SuperProp(p)=>self.super_get(p),
            ast::Expr::Paren(p) => self.lower(&p.expr),
            ast::Expr::Object(o) => {
                let target=self.temp();self.emit(o.span,Op::Set(target.clone(),Expr::Object(vec![])));
                for prop in &o.props {
                    let ast::PropOrSpread::Prop(prop)=prop else {
                        let ast::PropOrSpread::Spread(spread)=prop else{unreachable!()};
                        let value=self.lower(&spread.expr)?;
                        self.snapshot(spread.dot3_token,Expr::NodeBuiltin("$$spread".into(),vec![Expr::Variable(target.clone()),value]));continue;
                    };
                    if let ast::Prop::KeyValue(kv)=&**prop && (matches!(&kv.key,ast::PropName::Ident(i) if i.sym==*"__proto__") || matches!(&kv.key,ast::PropName::Str(s) if s.value.as_str()==Some("__proto__"))) {
                        let value=self.lower(&kv.value)?;self.snapshot(prop.span(),Expr::NodeBuiltin("$$literalPrototype".into(),vec![Expr::Variable(target.clone()),value]));continue;
                    }
                    let name=match &**prop{ast::Prop::KeyValue(p)=>Some(&p.key),ast::Prop::Method(p)=>Some(&p.key),ast::Prop::Getter(p)=>Some(&p.key),ast::Prop::Setter(p)=>Some(&p.key),_=>None};
                    let key=if let Some(ast::PropName::Computed(c))=name{let v=self.lower(&c.expr)?;self.snapshot(c.span,Expr::NodeBuiltin("$$propertyKey".into(),vec![v]))}else if let Some(name)=name{Expr::Value(json!(self.property(name)?))}else if let ast::Prop::Shorthand(i)=&**prop{Expr::Value(json!(i.sym.to_string()))}else{return self.error(prop.span(),"unsupported object property")};
                    let hint=if let Expr::Value(Value::String(s))=&key{s.clone()}else{String::new()};
                    let (value,access)=match &**prop {
                        ast::Prop::KeyValue(p)=>(self.lower_named_key(&p.value,Some(key.clone()))?,None),
                        ast::Prop::Shorthand(i)=>(self.expr(&ast::Expr::Ident(i.clone()))?,None),
                        ast::Prop::Method(m)=>(Expr::Function(self.compile_function(FunctionSource::method(&m.function,Some(hint)))?),None),
                        ast::Prop::Getter(g)=>{let source=FunctionSource{name:Some(format!("get {hint}")),self_name:None,params:vec![],body:g.body.as_ref().map(FunctionBody::Block).unwrap_or(FunctionBody::Missing),is_async:false,is_generator:false,lexical_this:false,constructable:false,span:g.span};(Expr::Function(self.compile_function(source)?),Some("get"))},
                        ast::Prop::Setter(g)=>{let source=FunctionSource{name:Some(format!("set {hint}")),self_name:None,params:vec![&g.param],body:g.body.as_ref().map(FunctionBody::Block).unwrap_or(FunctionBody::Missing),is_async:false,is_generator:false,lexical_this:false,constructable:false,span:g.span};(Expr::Function(self.compile_function(source)?),Some("set"))},
                        _=>return self.error(prop.span(),"unsupported object property"),
                    };
                    let value=if matches!(&**prop,ast::Prop::Method(_)|ast::Prop::Getter(_)|ast::Prop::Setter(_)){let value=self.snapshot(prop.span(),Expr::NodeBuiltin("$$methodHome".into(),vec![value,Expr::Variable(target.clone())]));self.snapshot(prop.span(),Expr::NodeBuiltin("$$functionName".into(),vec![value,key.clone(),Expr::Value(json!(access.unwrap_or("")))]))}else{value};
                    let mut args=vec![Expr::Variable(target.clone()),key,value];let name=if let Some(access)=access{args.push(Expr::Value(json!(access)));"$$literalAccessor"}else{"$$createData"};
                    self.snapshot(prop.span(),Expr::NodeBuiltin(name.into(),args));
                }
                Ok(Expr::Variable(target))
            }
            ast::Expr::Array(a) if a.elems.iter().any(|item| item.as_ref().is_some_and(|item| item.spread.is_some())) => {
                let target = self.temp();
                self.emit(a.span, Op::Set(target.clone(), Expr::Array(Vec::new())));
                for item in &a.elems {
                    let Some(item) = item else {
                        let destination = self.temp();
                        self.emit(a.span, Op::Append { target: target.clone(), items: vec![Expr::Undefined], destination });
                        let key = Expr::Binary("-".into(), Box::new(Expr::Member(Box::new(Expr::Variable(target.clone())), Box::new(Expr::Value(json!("length"))))), Box::new(Expr::Value(json!(1))));
                        self.snapshot(a.span, Expr::NodeBuiltin("delete".into(), vec![Expr::Variable(target.clone()), key]));
                        continue;
                    };
                    let value = self.lower(&item.expr)?;
                    if item.spread.is_some() {
                        let value = self.snapshot(item.span(), value);
                        let source = Expr::NodeBuiltin("$$iterableArray".into(), vec![value]);
                        let combined = Expr::StringMethod(Box::new(Expr::Variable(target.clone())), "concat".into(), vec![source]);
                        self.emit(item.span(), Op::Set(target.clone(), combined));
                    } else {
                        let destination = self.temp();
                        self.emit(item.span(), Op::Append { target: target.clone(), items: vec![value], destination });
                    }
                }
                Ok(Expr::Variable(target))
            }
            ast::Expr::Array(a) => {
                let mut values = Vec::new();
                for e in &a.elems {
                    match e {
                        Some(e) if e.spread.is_none() => {
                            let value = self.lower(&e.expr)?;
                            values.push(self.snapshot(e.span(), value));
                        }
                        None => values.push(Expr::Undefined),
                        _ => unreachable!(),
                    }
                }
                if a.elems.iter().all(Option::is_some) { return Ok(Expr::Array(values)); }
                let array = self.snapshot(a.span, Expr::Array(values));
                for (i, item) in a.elems.iter().enumerate() {
                    if item.is_none() { self.snapshot(a.span, Expr::NodeBuiltin("delete".into(), vec![array.clone(), Expr::Value(json!(i))])); }
                }
                Ok(array)
            }
            ast::Expr::Member(m) if self.constant_member(m).is_some() => Ok(self.constant_member(m).unwrap()),
            ast::Expr::Member(m) if self.environment_variable(m).is_some() => {
                let name = self.environment_variable(m).unwrap();
                Ok(Expr::NodeBuiltin("process.env".into(), vec![Expr::Value(json!(name))]))
            }
            ast::Expr::Member(m) if matches!(&*m.obj, ast::Expr::Ident(i) if i.sym == *"require") && self.global("require")
                && matches!(&m.prop, ast::MemberProp::Ident(p) if p.sym == *"main") => Ok(Expr::Undefined),
            ast::Expr::Arrow(arrow) => Ok(Expr::Function(
                self.compile_function(FunctionSource::arrow(arrow))?,
            )),
            ast::Expr::Fn(f) => {
                let name = f.ident.as_ref().map(|i| i.sym.to_string());
                Ok(Expr::Function(self.compile_function(FunctionSource::function(
                    &f.function,
                    name.clone(),
                    name,
                ))?))
            }
            ast::Expr::Member(m) => {
                if let Some(native) = self.native_value(e)? {
                    return Ok(native);
                }
                let object = self.lower_object(&m.obj)?;
                let object = self.snapshot(m.obj.span(), object);
                let key = match &m.prop {
                    ast::MemberProp::Ident(i) => Expr::Value(json!(i.sym.to_string())),
                    ast::MemberProp::PrivateName(i) => self.private_key(i)?,
                    ast::MemberProp::Computed(c) => self.lower(&c.expr)?,
                };
                Ok(Expr::Member(Box::new(object), Box::new(key)))
            }
            _ => self.expr(e),
        }
    }
    fn value(&mut self, e: &ast::Expr, dest: String) -> Result<()> {
        let value = self.lower(e)?;
        self.emit(e.span(), Op::Set(dest, value));
        Ok(())
    }
    fn discard(&mut self, e: &ast::Expr) -> Result<()> {
        // Even an unused expression must run and report arithmetic/type errors.
        let value = self.lower(e)?;
        self.snapshot(e.span(), value);
        Ok(())
    }
    /// Evaluate the source once, then initialize the pattern left-to-right.
    fn binding(&mut self, p: &ast::Pat, e: &ast::Expr, declare: bool) -> Result<()> {
        let value = self.lower_named(e,if let ast::Pat::Ident(i)=p{Some(i.id.sym.as_ref())}else{None})?;
        let value = self.snapshot(e.span(), value);
        self.bind_value(p, value, declare)
    }
    fn default_value(&mut self, span: Span, value: Expr, default: &ast::Expr, hint:Option<&str>) -> Result<Expr> {
        let name = self.temp();
        self.emit(span, Op::Set(name.clone(), value));
        let test = Expr::Binary("===".into(), Box::new(Expr::Variable(name.clone())), Box::new(Expr::Undefined));
        let skip = self.emit(span, Op::Branch(test, 0));
        let replacement = self.lower_named(default,hint)?;
        self.emit(span, Op::Set(name.clone(), replacement));
        self.land(skip);
        Ok(Expr::Variable(name))
    }
    fn binding_place(&mut self, p: &ast::Pat) -> Result<Option<(Expr, Expr, Option<Expr>)>> {
        let expression = match p {
            ast::Pat::Assign(a) => return self.binding_place(&a.left),
            ast::Pat::Rest(r) => return self.binding_place(&r.arg),
            ast::Pat::Expr(e) => &**e,
            _ => return Ok(None),
        };
        let mut e = expression;
        while let ast::Expr::Paren(p) = e { e = &p.expr; }
        match e {
            ast::Expr::Member(m) => {
                let value = self.lower(&m.obj)?;
                let object = self.snapshot(m.obj.span(), value);
                let key = self.member_key(&m.prop, m.span)?;
                let key = self.snapshot(m.span, Expr::NodeBuiltin("$$propertyKey".into(), vec![key]));
                Ok(Some((object, key, None)))
            }
            ast::Expr::SuperProp(p) => { let (base, key, receiver) = self.super_reference(p)?; Ok(Some((base, key, Some(receiver)))) }
            ast::Expr::Ident(i) => { self.local_target(i.sym.as_ref(), i.span)?; Ok(None) }
            _ => self.error(e.span(), "unsupported destructuring assignment target"),
        }
    }
    fn bind_value(&mut self, p: &ast::Pat, value: Expr, declare: bool) -> Result<()> {
        let place = self.binding_place(p)?;
        self.bind_prepared(p, value, declare, place)
    }
    fn bind_prepared(&mut self, p: &ast::Pat, value: Expr, declare: bool, place: Option<(Expr, Expr, Option<Expr>)>) -> Result<()> {
        match p {
            ast::Pat::Expr(e) => {
                if let Some((object, key, receiver)) = place {
                    if let Some(receiver) = receiver { self.class_helper(p.span(), "$$superSet", vec![object, key, value, receiver]); }
                    else { self.emit(p.span(), Op::SetMember { object, key, value }); }
                } else {
                    let mut e = &**e;
                    while let ast::Expr::Paren(p) = e { e = &p.expr; }
                    let ast::Expr::Ident(i) = e else { return self.error(e.span(), "invalid assignment target"); };
                    self.local_target(i.sym.as_ref(), i.span)?;
                    self.emit(i.span, Op::Set(i.sym.to_string(), value));
                }
            }
            ast::Pat::Ident(i) => {
                if !declare { self.local_target(i.id.sym.as_ref(), i.span())?; }
                let op = if declare { Op::Declare(i.id.sym.to_string(), value) } else { Op::Set(i.id.sym.to_string(), value) };
                self.emit(p.span(), op);
            }
            ast::Pat::Assign(a) => {
                let value = self.default_value(a.span, value, &a.right, if let ast::Pat::Ident(i)=&*a.left{Some(i.id.sym.as_ref())}else{None})?;
                self.bind_prepared(&a.left, value, declare, place)?;
            }
            ast::Pat::Array(a) => {
                let value = self.snapshot(a.span, value);
                let iterator=self.snapshot(a.span,Expr::NodeBuiltin("$$iterator".into(),vec![value]));
                let thrown=self.temp();let caught=self.emit(a.span,Op::Catch{target:0,name:thrown.clone()});self.handlers+=1;
                for index in 0..self.array_binding_len(a) {
                    let item = a.elems.get(index).unwrap_or(&None);
                    if let Some(item)=item {
                        let place = self.binding_place(item)?;
                        if let ast::Pat::Rest(rest)=item{let values=self.temp();self.emit(rest.span,Op::Effect(Some(values.clone()),Effect::Call("$$iteratorRest".into(),vec![iterator.clone()])));self.bind_prepared(&rest.arg,Expr::Variable(values),declare,place)?;}
                        else{let member=self.snapshot(item.span(),Expr::NodeBuiltin("$$iteratorValue".into(),vec![iterator.clone()]));self.bind_prepared(item,member,declare,place)?;}
                    }else{self.emit(a.span,Op::Effect(None,Effect::Call("$$iteratorElide".into(),vec![iterator.clone()])));}
                }
                self.handlers-=1;self.emit(a.span,Op::EndCatch);self.emit(a.span,Op::Effect(None,Effect::Call("$$iteratorCloseNormal".into(),vec![iterator.clone()])));
                let jump=self.emit(a.span,Op::Jump(0));let handler=self.code.len();self.emit(a.span,Op::Effect(None,Effect::Call("$$iteratorCloseThrow".into(),vec![iterator,Expr::Variable(thrown)])));
                let end=self.code.len();if let Op::Catch{target,..}=&mut self.code[caught].op{*target=handler;}self.code[jump].op=Op::Jump(end);
            }

            ast::Pat::Object(o) => {
                let value = self.snapshot(o.span, value);
                // Property access rejects null and undefined even for {}.
                self.snapshot(o.span, Expr::NodeBuiltin("$$requireCoercible".into(),vec![value.clone()]));
                let mut excluded = Vec::new();
                for prop in &o.props {
                    match prop {
                        ast::ObjectPatProp::KeyValue(k) => {
                            let key = match &k.key {
                                ast::PropName::Ident(i) => Expr::Value(json!(i.sym.to_string())),
                                ast::PropName::Str(v) => Expr::Value(json!(v.value.to_string_lossy().into_owned())),
                                ast::PropName::Num(v) => number_expr(v.value),
                                ast::PropName::Computed(v) => { let key = self.lower(&v.expr)?; self.snapshot(v.span, key) },
                                _ => return self.error(k.key.span(), "unsupported destructuring key"),
                            };
                            let key = self.snapshot(k.key.span(), Expr::NodeBuiltin("$$propertyKey".into(), vec![key]));
                            excluded.push(key.clone());
                            let place = self.binding_place(&k.value)?;
                            self.bind_prepared(&k.value, Expr::Member(Box::new(value.clone()), Box::new(key)), declare, place)?;
                        }
                        ast::ObjectPatProp::Assign(a) => {
                            let key = Expr::Value(json!(a.key.id.sym.to_string()));
                            excluded.push(key.clone());
                            let member = Expr::Member(Box::new(value.clone()), Box::new(key));
                            let member = if let Some(default) = &a.value { self.default_value(a.span, member, default,Some(a.key.id.sym.as_ref()))? } else { member };
                            let op = if declare { Op::Declare(a.key.id.sym.to_string(), member) } else { Op::Set(a.key.id.sym.to_string(), member) };
                            self.emit(a.span, op);
                        }
                        ast::ObjectPatProp::Rest(r) => {
                            let place = self.binding_place(&r.arg)?;
                            let copy = Expr::NodeBuiltin("$$objectRest".into(),vec![value.clone(),Expr::Array(excluded.clone())]);
                            let copy=self.snapshot(r.span,copy);
                            self.bind_prepared(&r.arg, copy, declare, place)?;
                        }
                    }
                }
            }
            _ => return self.error(p.span(), "unsupported destructuring pattern"),
        }
        Ok(())
    }
    fn loop_head(&mut self, head: &ast::ForHead, span: Span) -> Result<(ast::Pat, Option<ast::VarDeclKind>)> {
        match head {
            ast::ForHead::VarDecl(v) if v.decls.len() == 1 && v.decls[0].init.is_none() => Ok((v.decls[0].name.clone(), Some(v.kind))),
            ast::ForHead::Pat(pattern) => {
                for name in pattern_names(pattern) { self.local_target(&name, span)?; }
                Ok(((**pattern).clone(), None))
            }
            _ => self.error(span, "unsupported loop binding"),
        }
    }
    fn loop_bind(&mut self, pattern: &ast::Pat, value: Expr, declare: bool) -> Result<()> {
        self.bind_value(pattern, value, declare)
    }

    fn stmt(&mut self, s: &ast::Stmt) -> Result<()> {
        use ast::Stmt as S;
        match s {
            S::Block(b) => {
                let entered = self.enter_scope(b.span, lexical_names(&b.stmts))?;
                self.hoist_functions(&b.stmts)?;
                for s in &b.stmts {
                    self.stmt(s)?;
                }
                self.leave_scope(b.span, entered);
            }
            S::Empty(_) => {}
            S::Decl(ast::Decl::Var(v)) => self.variable_declaration(v)?,
            S::Decl(ast::Decl::Class(c)) => {let value=self.lower_class(&c.class,Some(c.ident.sym.to_string()))?;self.emit(c.class.span,Op::Declare(c.ident.sym.to_string(),value));},
            // Initialized when the enclosing scope was entered.
            S::Decl(ast::Decl::Fn(_)) => {}
            S::Expr(s) => {
                self.discard(&s.expr)?;
            }
            S::Return(r) => {
                let value = if let Some(e) = &r.arg {
                    self.lower(e)?
                } else {
                    Expr::Undefined
                };
                let value = if self.async_generator && r.arg.is_some() { self.await_value(value, r.span) } else { value };
                let value = if self.finalizers.is_empty() { value } else { self.snapshot(r.span, value) };
                self.emit_finalizers(None, r.span)?;
                self.emit(r.span, Op::Return(value));
            }
            S::If(i) => {
                let test = self.condition(&i.test)?;
                let branch = self.emit(i.span, Op::Branch(test, 0));
                self.stmt(&i.cons)?;
                let end = self.emit(i.span, Op::Jump(0));
                let alt = self.code.len();
                if let Some(a) = &i.alt {
                    self.stmt(a)?;
                }
                let done = self.code.len();
                if let Op::Branch(_, t) = &mut self.code[branch].op {
                    *t = alt;
                }
                self.code[end].op = Op::Jump(done);
            }
            S::While(w) => {
                let start = self.code.len();
                let test = self.condition(&w.test)?;
                let exit = self.emit(w.span, Op::Branch(test, 0));
                self.begin_loop();
                self.stmt(&w.body)?;
                self.emit(w.span, Op::Jump(start));
                let end = self.code.len();
                if let Op::Branch(_, target) = &mut self.code[exit].op {
                    *target = end;
                }
                self.end_loop(end, start);
            }
            S::DoWhile(w) => {
                let start = self.code.len();
                self.begin_loop();
                self.stmt(&w.body)?;
                let next = self.code.len();
                let test = self.condition(&w.test)?;
                let exit = self.emit(w.span, Op::Branch(test, 0));
                self.emit(w.span, Op::Jump(start));
                let end = self.code.len();
                if let Op::Branch(_, target) = &mut self.code[exit].op {
                    *target = end;
                }
                self.end_loop(end, next);
            }
            S::For(f) => {
                // `for (let …)` gets its own scope, copied for every iteration so
                // each iteration has fresh bindings.
                let mut entered = false;
                if let Some(init) = &f.init {
                    match init {
                        ast::VarDeclOrExpr::VarDecl(v) => {
                            if v.kind != ast::VarDeclKind::Var {
                                let names = v
                                    .decls
                                    .iter()
                                    .flat_map(|d| pattern_names(&d.name))
                                    .map(|name| (name, v.kind == ast::VarDeclKind::Let))
                                    .collect();
                                entered = self.enter_scope(f.span, names)?;
                            }
                            self.variable_declaration(v)?
                        }
                        ast::VarDeclOrExpr::Expr(e) => {
                            self.discard(e)?;
                        }
                    }
                }
                if entered {
                    self.emit(f.span, Op::CopyScope);
                }
                let start = self.code.len();
                let test = if let Some(test) = &f.test {
                    self.condition(test)?
                } else {
                    Expr::Value(json!(true))
                };
                let exit = self.emit(f.span, Op::Branch(test, 0));
                self.begin_loop();
                self.stmt(&f.body)?;
                let next = self.code.len();
                if entered {
                    self.emit(f.span, Op::CopyScope);
                }
                if let Some(update) = &f.update {
                    self.discard(update)?;
                }
                self.emit(f.span, Op::Jump(start));
                let end = self.code.len();
                if let Op::Branch(_, target) = &mut self.code[exit].op {
                    *target = end;
                }
                self.end_loop(end, next);
                self.leave_scope(f.span, entered);
            }
            S::Labeled(label) => {
                fn targets_loop(stmt: &ast::Stmt) -> bool {
                    match stmt {
                        ast::Stmt::For(_) | ast::Stmt::ForIn(_) | ast::Stmt::ForOf(_) | ast::Stmt::While(_) | ast::Stmt::DoWhile(_) => true,
                        ast::Stmt::Labeled(inner) => targets_loop(&inner.body),
                        _ => false,
                    }
                }
                let name = label.label.sym.to_string();
                if self.loops.iter().any(|context| context.label.as_deref() == Some(&name)) {
                    return self.error(label.span, "duplicate label");
                }
                self.begin_loop();
                let context = self.loops.last_mut().unwrap();
                context.is_switch = true;
                context.label = Some(name);
                context.label_loop = targets_loop(&label.body);
                self.stmt(&label.body)?;
                let end = self.code.len();
                self.end_loop(end, end);
            }
            S::Switch(switch) => {
                let discriminant = self.lower(&switch.discriminant)?;
                let discriminant = self.snapshot(switch.discriminant.span(), discriminant);
                let statements: Vec<_> = switch.cases.iter().flat_map(|case| case.cons.iter().cloned()).collect();
                let entered = self.enter_scope(switch.span, lexical_names(&statements))?;
                self.hoist_functions(&statements)?;
                let mut jumps = Vec::new();
                let mut default = None;
                for (index, case) in switch.cases.iter().enumerate() {
                    if let Some(test) = &case.test {
                        let value = self.lower(test)?;
                        let equal = Expr::Binary("===".into(), Box::new(discriminant.clone()), Box::new(value));
                        let next = self.emit(case.span, Op::Branch(equal, 0));
                        jumps.push((index, self.emit(case.span, Op::Jump(0))));
                        self.land(next);
                    } else { default = Some(index); }
                }
                let unmatched = self.emit(switch.span, Op::Jump(0));
                self.begin_loop();
                self.loops.last_mut().unwrap().is_switch = true;
                let mut locations = Vec::new();
                for case in &switch.cases {
                    locations.push(self.code.len());
                    for stmt in &case.cons { self.stmt(stmt)?; }
                }
                let end = self.code.len();
                for (index, jump) in jumps { self.code[jump].op = Op::Jump(locations[index]); }
                self.code[unmatched].op = Op::Jump(default.map(|i| locations[i]).unwrap_or(end));
                self.end_loop(end, end);
                self.leave_scope(switch.span, entered);
            }
            S::Break(b) => {
                let target = if let Some(label) = &b.label {
                    self.loops.iter().rposition(|context| context.label.as_deref() == Some(label.sym.as_ref()))
                } else {
                    self.loops.iter().rposition(|context| context.label.is_none())
                };
                let Some(target) = target else { return self.error(b.span, "break requires an enclosing loop, switch, or matching label"); };
                let remaining = self.emit_finalizers(Some(target), b.span)?;
                self.leave_to_loop(b.span, target, remaining);
                let at = self.emit(b.span, Op::Jump(0));
                self.loops[target].breaks.push(at);
            }
            S::Continue(c) => {
                let target = if let Some(label) = &c.label {
                    self.loops.iter().rposition(|context| context.label.as_deref() == Some(label.sym.as_ref()) && context.label_loop)
                        .and_then(|index| self.loops.iter().enumerate().skip(index + 1).find(|(_, context)| !context.is_switch).map(|(index, _)| index))
                } else {
                    self.loops.iter().rposition(|context| !context.is_switch)
                };
                let Some(target) = target else { return self.error(c.span, "continue requires an enclosing loop or matching loop label"); };
                let remaining = self.emit_finalizers(Some(target), c.span)?;
                self.leave_to_loop(c.span, target, remaining);
                let at = self.emit(c.span, Op::Jump(0));
                self.loops[target].continues.push(at);
            }
            S::ForIn(f) => {
                let (pattern, kind) = self.loop_head(&f.left, f.span)?;
                let head_scope = if matches!(kind, Some(ast::VarDeclKind::Let | ast::VarDeclKind::Const)) {
                    let names = pattern_names(&pattern).into_iter().map(|name| (name, kind == Some(ast::VarDeclKind::Let))).collect();
                    self.enter_scope(f.span, names)?
                } else { false };
                let items = self.temp();
                let index = self.temp();
                let item = self.temp();
                let source = self.lower(&f.right)?;
                let source = self.snapshot(f.right.span(), source);
                let is_null = Expr::Binary("==".into(), Box::new(source.clone()), Box::new(Expr::Value(Value::Null)));
                let keys = Expr::Conditional(Box::new(is_null), Box::new(Expr::Array(Vec::new())), Box::new(Expr::NodeBuiltin("$$forInKeys".into(), vec![source.clone()])));
                self.emit(f.span, Op::Set(items.clone(), keys));
                self.emit(f.span, Op::Set(index.clone(), Expr::Value(json!(0))));
                let start = self.code.len();
                let next = self.emit(f.span, Op::ForNext { items, index, item: item.clone(), end: 0 });
                // JS skips a property deleted before it is visited. Primitive
                // strings have immutable indexed properties.
                let object = Expr::Binary("===".into(), Box::new(Expr::Unary("typeof".into(), Box::new(source.clone()))), Box::new(Expr::Value(json!("object"))));
                let present = Expr::Binary("in".into(), Box::new(Expr::Variable(item.clone())), Box::new(source));
                self.emit(f.span, Op::Branch(Expr::Conditional(Box::new(object), Box::new(present), Box::new(Expr::Value(json!(true)))), start));
                self.begin_loop();
                let entered = if matches!(kind, Some(ast::VarDeclKind::Let | ast::VarDeclKind::Const)) {
                    let names = pattern_names(&pattern).into_iter().map(|name| (name, kind == Some(ast::VarDeclKind::Let))).collect();
                    self.enter_scope(f.span, names)?
                } else { false };
                self.loop_bind(&pattern, Expr::Variable(item), entered)?;
                self.stmt(&f.body)?;
                self.leave_scope(f.span, entered);
                self.emit(f.span, Op::Jump(start));
                let end = self.code.len();
                if let Op::ForNext { end: target, .. } = &mut self.code[next].op { *target = end; }
                self.end_loop(end, start);
                self.leave_scope(f.span, head_scope);
            }
            S::ForOf(f) => {
                let (pattern,kind)=self.loop_head(&f.left,f.span)?;
                let head_scope=if matches!(kind,Some(ast::VarDeclKind::Let|ast::VarDeclKind::Const)){self.enter_scope(f.span,pattern_names(&pattern).into_iter().map(|name|(name,kind==Some(ast::VarDeclKind::Let))).collect())?}else{false};
                let statement=self.iterable_for_of(f)?;self.stmt(&statement)?;self.leave_scope(f.span,head_scope);
            }

            S::Try(t) => {
                if let Some(finalizer) = &t.finalizer {
                    let context = FinallyContext {
                        block: finalizer.clone(), handlers: self.handlers,
                        runtime_scopes: self.runtime_scopes, scopes_len: self.scopes.len(), loops_len: self.loops.len(),
                    };
                    let thrown = self.temp();
                    let outer = self.emit(t.span, Op::Catch { target: 0, name: thrown.clone() });
                    self.handlers += 1;
                    self.finalizers.push(context);
                    if t.handler.is_some() {
                        let mut inner = t.clone();
                        inner.finalizer = None;
                        self.stmt(&ast::Stmt::Try(inner))?;
                    } else {
                        self.stmt(&ast::Stmt::Block(t.block.clone()))?;
                    }
                    self.finalizers.pop();
                    self.handlers -= 1;
                    self.emit(t.span, Op::EndCatch);
                    self.stmt(&ast::Stmt::Block(finalizer.clone()))?;
                    let end_jump = self.emit(t.span, Op::Jump(0));
                    let handler = self.code.len();
                    self.stmt(&ast::Stmt::Block(finalizer.clone()))?;
                    self.emit(t.span, Op::Throw(Expr::Variable(thrown)));
                    let end = self.code.len();
                    if let Op::Catch { target, .. } = &mut self.code[outer].op { *target = handler; }
                    self.code[end_jump].op = Op::Jump(end);
                    return Ok(());
                }
                let h = t.handler.as_ref().context("try requires catch")?;
                let param = h.param.as_ref();
                // The handler receives the thrown value in a temporary; the
                // catch parameter is a binding of the handler's own scope.
                let thrown = self.temp();
                let start = self.emit(
                    t.span,
                    Op::Catch {
                        target: 0,
                        name: thrown.clone(),
                    },
                );
                self.handlers += 1;
                let entered = self.enter_scope(t.block.span, lexical_names(&t.block.stmts))?;
                self.hoist_functions(&t.block.stmts)?;
                for s in &t.block.stmts {
                    self.stmt(s)?;
                }
                self.leave_scope(t.block.span, entered);
                self.handlers -= 1;
                self.emit(t.span, Op::EndCatch);
                let jump = self.emit(t.span, Op::Jump(0));
                let handler = self.code.len();
                let mut names: Vec<(String, bool)> = param
                    .map(pattern_names).unwrap_or_default().into_iter().map(|name| (name, true)).collect();
                names.extend(lexical_names(&h.body.stmts));
                let entered = self.enter_scope(h.span, names)?;
                if let Some(param) = param {
                    self.bind_value(param, Expr::Variable(thrown), true)?;
                }
                self.hoist_functions(&h.body.stmts)?;
                for s in &h.body.stmts {
                    self.stmt(s)?;
                }
                self.leave_scope(h.span, entered);
                let end = self.code.len();
                if let Op::Catch { target, .. } = &mut self.code[start].op {
                    *target = handler;
                }
                self.code[jump].op = Op::Jump(end);
            }
            S::Throw(t) => {
                let value = self.lower(&t.arg)?;
                self.emit(t.span, Op::Throw(value));
            }
            _ => return self.error(
                s.span(),
                "unsupported statement; use declarations, if, for...of, return and try/catch/throw",
            ),
        }
        Ok(())
    }
}

#[cfg(test)]
pub fn compile(source: &str) -> Result<Program> {
    compile_with_libraries(source, super::libraries::builtin())
}
#[cfg(test)]
pub(super) fn compile_conformance(source: &str) -> Result<Program> {
    let names=["assert","__test262CheckThrown","$DONE","compareArray"]
        .into_iter().map(str::to_owned).collect();
    link_prelude(compile_inner(source,super::ENTRY,false,super::libraries::Libraries::new(),names,false)?)
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
pub fn compile_with_libraries(
    source: &str,
    libraries: super::libraries::Libraries,
) -> Result<Program> {
    link_prelude(compile_inner(source, super::ENTRY, false, libraries, BTreeSet::new(), false)?)
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
    let program = link_prelude(compile_inner(source, entry, true, libraries, external_names, false)?)?;
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
    link_prelude(compile_inner(source, entry, false, libraries, BTreeSet::new(), false)?)
}
fn compile_inner(
    source: &str,
    entry: &str,
    refinement: bool,
    libraries: super::libraries::Libraries,
    external_names: BTreeSet<String>,
    internal: bool,
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
        Syntax::Es(EsSyntax::default()),
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
            "ReturnNotAllowed" => "return is only allowed inside a function; a program ends after its last statement, so log what it found with console.log".to_owned(),
            _ => kind,
        }
    };
    let module = parser.parse_module().map_err(|e| {
        let p = map.lookup_char_pos(e.span().lo);
        anyhow::anyhow!("TS_PROGRAM {}:{}: {}", p.line, p.col.0 + 1, describe(format!("{:?}", e.kind())))
    })?;
    if let Some(e) = parser.take_errors().first() {
        let p = map.lookup_char_pos(e.span().lo);
        bail!("TS_PROGRAM {}:{}: {}", p.line, p.col.0 + 1, describe(format!("{:?}", e.kind())));
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
            ast::ModuleItem::Stmt(stmt) if refinement || entry == super::ENTRY => top_level.push(stmt),
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
    let implicit_entry = !refinement && entry == super::ENTRY;
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
        names.insert(super::ENTRY.into());
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
        arities.insert(super::ENTRY.into(), 0);
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
    // that source is a main program or a module.
    let leaf_tools = Some(imported_tools.clone());
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
        let init_id = if implicit_entry { super::ENTRY } else { super::MODULE_INIT };
        let module_vars = var_names(&top_level);
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
        for stmt in &top_level {
            c.stmt(stmt)?;
        }
        c.emit(module.span, Op::Return(Expr::Undefined));
        functions.extend(std::mem::take(&mut c.nested));
        functions.insert(
            init_id.into(),
            Function {
                uses: Vec::new(),
                description: String::new(),
                virtual_task: false,
                module_task: false,
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
        if virtual_task && !parameter_patterns.is_empty() {
            return c.error(f.function.span, "@virtual and @module functions require simple parameters");
        }
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
        let empty = body.stmts.iter().all(|s| matches!(s, ast::Stmt::Empty(_)));
        // A sole `throw new Error("literal")` is a conventional unavailable-implementation
        // sentinel. Do not ignore arbitrary throws, expressions or side effects.
        let executable = body
            .stmts
            .iter()
            .filter(|s| !matches!(s, ast::Stmt::Empty(_)))
            .collect::<Vec<_>>();
        let error_placeholder = matches!(executable.as_slice(), [ast::Stmt::Throw(t)]
            if matches!(&*t.arg, ast::Expr::New(n)
                if matches!(&*n.callee, ast::Expr::Ident(i) if i.sym == *"Error")
                    && n.args.as_ref().is_some_and(|args| args.len() == 1
                        && args[0].spread.is_none()
                        && matches!(&*args[0].expr, ast::Expr::Lit(ast::Lit::Str(_))))));
        if virtual_task {
            if !empty && !error_placeholder {
                return c.error(body.span,
                    "@virtual and @module declare AI work: use an empty body or a sole throw new Error with a literal message; executable work and placeholder returns are unsupported");
            }
            if description.is_empty() {
                return c.error(
                    body.span,
                    "@virtual and @module need a task description and return contract",
                );
            }
            let uses = if module_task {
                c.no_uses(&description, f.function.span)?;
                Vec::new()
            } else {
                c.captures(&description, &params, f.function.span)?
            };
            let input = Expr::Object(
                params
                    .iter()
                    .chain(&uses)
                    .map(|p| (p.clone(), Expr::Variable(p.clone())))
                    .collect(),
            );
            let dest = c.temp();
            c.emit(
                body.span,
                Op::Effect(
                    Some(dest.clone()),
                    Effect::Task {
                        name: f.ident.sym.to_string(),
                        description: description.clone(),
                        input,
                    },
                ),
            );
            c.emit(body.span, Op::Return(Expr::Variable(dest)));
        } else {
            c.hoist_functions(&body.stmts)?;
            for s in &body.stmts {
                c.stmt(s)?;
            }
            c.emit(body.span, Op::Return(Expr::Undefined));
        }
        functions.extend(std::mem::take(&mut c.nested));
        let uses = if virtual_task && !module_task { c.captures(&description, &params, f.function.span)? } else { Vec::new() };
        functions.insert(
            f.ident.sym.to_string(),
            Function {
                description,
                virtual_task,
                module_task,
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
    compile_inner(super::prelude::SOURCE, "$$map", false, super::libraries::Libraries::new(), BTreeSet::new(), true)
}
