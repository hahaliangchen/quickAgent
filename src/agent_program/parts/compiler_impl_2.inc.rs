impl Compiler {
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
            ast::Expr::New(new) if matches!(&*new.callee, ast::Expr::Ident(i) if i.sym == *"URL" && self.global("URL")) => {
                let args = new.args.as_deref().unwrap_or(&[]);
                if args.len() > 2 || args.iter().any(|arg| arg.spread.is_some()) {
                    return self.error(new.span, "URL takes an input and an optional base URL");
                }
                let values = args.iter().map(|arg| self.lower(&arg.expr).map(|value| self.snapshot(arg.span(), value))).collect::<Result<Vec<_>>>()?;
                Ok(self.snapshot(new.span, Expr::NodeBuiltin("new URL".into(), values)))
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
                    if call.args.len() != 1 || call.args[0].spread.is_some() {
                        return self.error(call.span, "require takes exactly one library name");
                    }
                    if let Some(library) = required_name(call) {
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
                    let value = self.lower(&call.args[0].expr)?;
                    let destination = self.temp();
                    self.emit(call.span, Op::Effect(Some(destination.clone()), Effect::RequireValue(value)));
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
                let value = if self.segment_root || !self.finalizers.is_empty() {
                    self.snapshot(r.span, value)
                } else {
                    value
                };
                if self.segment_root {
                    self.emit(r.span, Op::Set("\0explicit_return".into(), Expr::Value(json!(true))));
                    self.emit(r.span, Op::Set("$_".into(), value.clone()));
                }
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
