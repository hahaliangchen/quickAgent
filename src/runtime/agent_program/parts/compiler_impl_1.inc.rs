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
                    return self.error(i.span,&format!("{} cannot be used as a function value in this runtime; call {}(...) or new {}(...) directly",i.sym,i.sym,i.sym));
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
                let name=match &*new.callee {E::Ident(i)=>i.sym.to_string(),_=>"<expression>".into()};
                let alternative=match name.as_str() {
                    "Map" | "WeakMap"=>"; a plain object can hold string keys, but not object keys or reference identity",
                    "Set" | "WeakSet"=>"; an array with includes handles a small set, but not all Set semantics",
                    _=>"",
                };
                if matches!(name.as_str(), "TextDecoder" | "TextDecoderStream") {
                    return self.error(new.span, &super::unsupported_builtin(&name));
                }
                return self.error(new.span,&format!("new {name}(...) is not supported in this runtime yet{alternative}."));
            }
            E::This(_)=>Expr::Variable("\0this".into()),
            E::Class(node)=>return self.error(node.class.span,"class expressions are not supported in this runtime yet; declare the class with a class statement"),
            E::Yield(node)=>return self.error(node.span,"yield must pass through the resumable expression boundary"),
            E::TaggedTpl(node) => return self.error(node.span, "tagged templates must be evaluated as expressions"),
            E::MetaProp(node) if node.kind==ast::MetaPropKind::NewTarget=>Expr::Variable("\0newTarget".into()),
            E::MetaProp(node)=>return self.error(node.span,"import.meta is not supported in this runtime"),
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
            segment_root: false,
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
        let mut uses = Vec::new();
        let mut partial_virtual = false;
        match source.body {
            FunctionBody::Block(body) if contract.is_some() => {
                let (_, _, description) = contract.clone().unwrap();
                let (empty, error_placeholder) = block_is_virtual_placeholder(&body);
                if description.is_empty() {
                    return c.error(body.span, "@virtual and @module need a task description and return contract");
                }
                if module_task && !empty && !error_placeholder {
                    return c.error(body.span, "@module declares reusable AI work: use an empty body or a sole throw new Error with a literal message");
                }
                uses = if module_task {
                    self.no_uses(&description, source.span)?;
                    Vec::new()
                } else {
                    self.captures(&description, &params, source.span)?
                };
                partial_virtual = !module_task && !empty && !error_placeholder;
                if partial_virtual {
                    c.hoist_functions(&body.stmts)?;
                    for s in &body.stmts {
                        c.stmt(s)?;
                    }
                }
                emit_virtual_task_tail(&mut c, body.span, &id, description, &params, &uses);
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
        self.nested.push((
            id.clone(),
            Function {
                description,
                virtual_task,
                partial_virtual,
                module_task,
                explicit_module: module_task,
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
                // Support Node-style `<lib>.promises.<method>` (e.g. `io.promises.read`,
                // `process.promises.run_readonly`, `git.promises.status`) for Promise.all.
                if let (ast::Expr::Member(inner), ast::MemberProp::Ident(method)) = (&*m.obj, &m.prop)
                    && let (ast::Expr::Ident(namespace), ast::MemberProp::Ident(sub)) = (&*inner.obj, &inner.prop)
                    && sub.sym.as_ref() == "promises"
                    && self.resolve(namespace.sym.as_ref()).is_none()
                    && let Some(methods) = self.libraries.get(namespace.sym.as_ref())
                {
                    return match methods.get(method.sym.as_ref()) {
                        Some(tool) => Ok(Some(Expr::Native(format!("tool:async-tool:{tool}")))),
                        None => self.error(
                            m.span,
                            &format!(
                                "unknown async library method {}.promises.{}; use the published library catalog",
                                namespace.sym, method.sym
                            ),
                        ),
                    };
                }
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
                    (matches!(namespace, "Math" | "Number" | "String" | "Date" | "Object" | "Array" | "JSON" | "Reflect" | "Symbol" | "Map" | "RegExp" | "Promise")
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
                    // Worded as JS says it, so the author recognises the mistake.
                    let alternative = match name.sym.as_ref() {
                        "print" | "println" => "use console.log(...); the other log methods are in read_program_guide",
                        _ => "declare it in this source (AI work is an empty function with a @virtual contract) or use a built-in listed in read_program_guide",
                    };
                    return self.error(call.span, &format!("{} is not defined: {alternative}", name.sym));
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
                if !matches!(namespace.sym.as_ref(), "Math" | "Number" | "String" | "Date" | "Object" | "Array" | "JSON" | "Reflect" | "Symbol" | "Map" | "RegExp" | "Promise")
                    || !self.global(namespace.sym.as_ref())
                    || namespace.sym == *"Promise" && method.sym != *"withResolvers"
                {
                    return Ok(None);
                }
                format!("{}.{}", namespace.sym, method.sym)
            }
            _ => return Ok(None),
        };
        let Some((min, max)) = super::js_methods::builtin_arity(&name) else {
            return match &**callee {
                ast::Expr::Member(_) => self.error(call.span, &super::unsupported_builtin(&name)),
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

}
