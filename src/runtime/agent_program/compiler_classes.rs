//! Classes lower to ordinary durable closures. Home objects and constructor
//! initialization live in the heap/runtime, never in a host closure.
use super::*;

impl Compiler {
    pub(super) fn class_helper(&mut self, span: Span, name: &str, args: Vec<Expr>) -> Expr {
        let dest = self.temp();
        self.emit(
            span,
            Op::Effect(Some(dest.clone()), Effect::Call(name.into(), args)),
        );
        Expr::Variable(dest)
    }

    pub(super) fn lower_named(&mut self, e: &ast::Expr, hint: Option<&str>) -> Result<Expr> {
        self.lower_named_key(e, hint.map(|s| Expr::Value(json!(s))))
    }
    pub(super) fn lower_named_key(&mut self, e: &ast::Expr, hint: Option<Expr>) -> Result<Expr> {
        match e {
            ast::Expr::Paren(p) => self.lower_named_key(&p.expr, hint),
            ast::Expr::Class(c) if c.ident.is_none() => {
                self.lower_class_named(&c.class, None, hint)
            }
            ast::Expr::Fn(f) if f.ident.is_none() && hint.is_some() => {
                let id =
                    self.compile_function(FunctionSource::function(&f.function, None, None))?;
                Ok(self.snapshot(
                    e.span(),
                    Expr::NodeBuiltin(
                        "$$functionName".into(),
                        vec![Expr::Function(id), hint.unwrap(), Expr::Value(json!(""))],
                    ),
                ))
            }
            ast::Expr::Arrow(a) if hint.is_some() => {
                let id = self.compile_function(FunctionSource::arrow(a))?;
                Ok(self.snapshot(
                    e.span(),
                    Expr::NodeBuiltin(
                        "$$functionName".into(),
                        vec![Expr::Function(id), hint.unwrap(), Expr::Value(json!(""))],
                    ),
                ))
            }
            _ => self.lower(e),
        }
    }
    pub(super) fn lower_class(&mut self, class: &ast::Class, name: Option<String>) -> Result<Expr> {
        self.lower_class_named(class, name, None)
    }
    fn lower_class_named(
        &mut self,
        class: &ast::Class,
        name: Option<String>,
        inferred: Option<Expr>,
    ) -> Result<Expr> {
        let saved_private = self.private_names.clone();
        let prefix = format!("\0class:{}:", class.span.lo.0);
        let mut bindings: Vec<(String, bool)> = name.iter().map(|n| (n.clone(), true)).collect();
        let mut private = BTreeMap::new();
        let mut declarations: BTreeMap<String, (bool, u8)> = BTreeMap::new();
        for member in &class.body {
            let declaration = match member {
                ast::ClassMember::PrivateProp(p) => Some((p.key.name.to_string(), p.is_static, 4)),
                ast::ClassMember::PrivateMethod(m) => Some((
                    m.key.name.to_string(),
                    m.is_static,
                    match m.kind {
                        ast::MethodKind::Getter => 1,
                        ast::MethodKind::Setter => 2,
                        ast::MethodKind::Method => 4,
                    },
                )),
                _ => None,
            };
            if let Some((key, is_static, kind)) = declaration {
                if let Some((previous_static, previous_kind)) = declarations.get_mut(&key) {
                    if *previous_static != is_static
                        || *previous_kind & kind != 0
                        || *previous_kind | kind != 3
                    {
                        return self.error(
                            member.span(),
                            &format!("duplicate private declaration #{key}"),
                        );
                    }
                    *previous_kind |= kind;
                } else {
                    declarations.insert(key, (is_static, kind));
                }
            }
            let key = match member {
                ast::ClassMember::PrivateProp(p) => Some(p.key.name.to_string()),
                ast::ClassMember::PrivateMethod(m) => Some(m.key.name.to_string()),
                _ => None,
            };
            if let Some(key) = key {
                let hidden = format!("{prefix}private:{key}");
                if !private.contains_key(&key) {
                    bindings.push((hidden.clone(), false));
                }
                private.insert(key, hidden);
            }
        }
        for index in 0..class.body.len() {
            bindings.push((format!("{prefix}key:{index}"), false));
            bindings.push((format!("{prefix}method:{index}"), false));
        }
        bindings.push((format!("{prefix}initialize"), false));
        let entered = self.enter_scope(class.span, bindings)?;
        // Heritage uses the surrounding private environment, not this class's.
        let parent = if let Some(parent) = &class.super_class {
            let value = self.lower(parent)?;
            self.snapshot(parent.span(), value)
        } else {
            Expr::Undefined
        };
        self.private_names.extend(private.clone());
        for (key, hidden) in private {
            let value = self.snapshot(
                class.span,
                Expr::NodeBuiltin(
                    "$$privateKey".into(),
                    vec![Expr::Value(json!(format!("#{key}")))],
                ),
            );
            self.emit(class.span, Op::Declare(hidden, value));
        }
        let derived = class.super_class.is_some();
        let explicit = class.body.iter().find_map(|m| {
            if let ast::ClassMember::Constructor(c) = m {
                Some(c)
            } else {
                None
            }
        });
        let default;
        let source = if let Some(c) = explicit {
            FunctionSource {
                name: name.clone(),
                self_name: None,
                params: c
                    .params
                    .iter()
                    .map(|p| match p {
                        ast::ParamOrTsParamProp::Param(p) => Ok(&p.pat),
                        _ => self.error(c.span, "TypeScript parameter properties are unsupported"),
                    })
                    .collect::<Result<Vec<_>>>()?,
                body: c
                    .body
                    .as_ref()
                    .map(FunctionBody::Block)
                    .unwrap_or(FunctionBody::Missing),
                is_async: false,
                is_generator: false,
                lexical_this: false,
                constructable: true,
                span: c.span,
            }
        } else {
            let text = if derived {
                "function __default(...args){return $$superCallArray(args);}"
            } else {
                "function __default(){}"
            };
            let file = self.map.new_source_file(
                FileName::Custom("<default constructor>".into()).into(),
                text.to_owned(),
            );
            let lexer = Lexer::new(
                Syntax::Es(EsSyntax::default()),
                Default::default(),
                StringInput::from(&*file),
                None,
            );
            let stmt = Parser::new_from(lexer)
                .parse_stmt()
                .map_err(|e| anyhow::anyhow!("default constructor: {:?}", e.kind()))?;
            let ast::Stmt::Decl(ast::Decl::Fn(f)) = stmt else {
                unreachable!()
            };
            default = f.function;
            let mut source = FunctionSource::function(&default, name.clone(), None);
            source.span = class.span;
            source
        };
        let id = self.compile_function(source)?;
        let ctor = self.class_helper(
            class.span,
            "$$createClass",
            vec![
                Expr::Function(id),
                parent,
                Expr::Value(json!(derived)),
                name.clone()
                    .map(|n| Expr::Value(json!(n)))
                    .or(inferred)
                    .unwrap_or_else(|| Expr::Value(json!(""))),
            ],
        );
        let mut instance = Vec::new();
        let mut statics = Vec::new();
        let mut private_methods: BTreeMap<
            (bool, String),
            (Expr, Option<Expr>, Option<Expr>, Span),
        > = BTreeMap::new();
        // Evaluate all computed keys once, in source order. Initializers run later.
        for (index, member) in class.body.iter().enumerate() {
            match member {
                ast::ClassMember::Constructor(_) | ast::ClassMember::Empty(_) => {}
                ast::ClassMember::Method(m) => {
                    let key = self.class_element_key(&m.key, &format!("{prefix}key:{index}"))?;
                    let function = self.compile_function(FunctionSource::method(
                        &m.function,
                        Some(format!("method:{index}")),
                    ))?;
                    let kind = match m.kind {
                        ast::MethodKind::Method => "value",
                        ast::MethodKind::Getter => "get",
                        ast::MethodKind::Setter => "set",
                    };
                    self.snapshot(
                        m.span,
                        Expr::NodeBuiltin(
                            "$$classMethod".into(),
                            vec![
                                ctor.clone(),
                                key,
                                Expr::Function(function),
                                Expr::Value(json!(kind)),
                                Expr::Value(json!(m.is_static)),
                            ],
                        ),
                    );
                }
                ast::ClassMember::ClassProp(p) => {
                    let key = self.class_element_key(&p.key, &format!("{prefix}key:{index}"))?;
                    let entry = ClassElement::Field {
                        span: p.span,
                        key: key.clone(),
                        hint: key,
                        value: p.value.as_deref(),
                        private: false,
                    };
                    if p.is_static {
                        statics.push(entry);
                    } else {
                        instance.push(entry);
                    }
                }
                ast::ClassMember::PrivateProp(p) => {
                    let entry = ClassElement::Field {
                        span: p.span,
                        key: self.private_key(&p.key)?,
                        hint: Expr::Value(json!(format!("#{}", p.key.name))),
                        value: p.value.as_deref(),
                        private: true,
                    };
                    if p.is_static {
                        statics.push(entry);
                    } else {
                        instance.push(entry);
                    }
                }
                ast::ClassMember::PrivateMethod(m) => {
                    let key = self.private_key(&m.key)?;
                    let function = self.compile_function(FunctionSource::method(
                        &m.function,
                        Some(format!("private:{index}")),
                    ))?;
                    let kind = match m.kind {
                        ast::MethodKind::Method => "method",
                        ast::MethodKind::Getter => "get",
                        ast::MethodKind::Setter => "set",
                    };
                    let method = self.snapshot(
                        m.span,
                        Expr::NodeBuiltin(
                            "$$privateMethod".into(),
                            vec![
                                ctor.clone(),
                                Expr::Function(function),
                                Expr::Value(json!(format!("#{}", m.key.name))),
                                Expr::Value(json!(m.is_static)),
                                Expr::Value(json!(if kind == "method" { "" } else { kind })),
                            ],
                        ),
                    );
                    let hidden = format!("{prefix}method:{index}");
                    self.emit(m.span, Op::Declare(hidden.clone(), method));
                    let function = Expr::Variable(hidden);
                    if kind == "method" {
                        let entry = ClassElement::Private {
                            span: m.span,
                            key,
                            kind: "method",
                            value: function,
                            setter: Expr::Undefined,
                        };
                        if m.is_static {
                            statics.insert(0, entry);
                        } else {
                            instance.insert(0, entry);
                        }
                    } else {
                        let e = private_methods
                            .entry((m.is_static, m.key.name.to_string()))
                            .or_insert((key, None, None, m.span));
                        if kind == "get" {
                            e.1 = Some(function);
                        } else {
                            e.2 = Some(function);
                        }
                    }
                }
                ast::ClassMember::StaticBlock(block) => {
                    let source = FunctionSource {
                        name: Some(format!("static-block:{index}")),
                        self_name: None,
                        params: vec![],
                        body: FunctionBody::Block(&block.body),
                        is_async: false,
                        is_generator: false,
                        lexical_this: false,
                        constructable: false,
                        span: block.span,
                    };
                    let id = self.compile_function(source)?;
                    let function = self.snapshot(
                        block.span,
                        Expr::NodeBuiltin(
                            "$$methodHome".into(),
                            vec![Expr::Function(id), ctor.clone()],
                        ),
                    );
                    let hidden = format!("{prefix}method:{index}");
                    self.emit(block.span, Op::Declare(hidden.clone(), function));
                    statics.push(ClassElement::Block {
                        span: block.span,
                        function: Expr::Variable(hidden),
                    });
                }
                _ => return self.error(member.span(), "unsupported class element"),
            }
        }
        for ((is_static, _), (key, get, set, span)) in private_methods {
            let entry = ClassElement::Private {
                span,
                key,
                kind: "accessor",
                value: get.unwrap_or(Expr::Undefined),
                setter: set.unwrap_or(Expr::Undefined),
            };
            if is_static {
                statics.insert(0, entry);
            } else {
                instance.insert(0, entry);
            }
        }
        if let Some(name) = name {
            self.emit(class.span, Op::Declare(name.clone(), ctor.clone()));
            self.snapshot(
                class.span,
                Expr::NodeBuiltin(
                    "$$classBinding".into(),
                    vec![ctor.clone(), Expr::Value(json!(name))],
                ),
            );
        }
        let id = self.compile_class_elements(class.span, "instance-elements", instance)?;
        let init = self.snapshot(
            class.span,
            Expr::NodeBuiltin(
                "$$methodHome".into(),
                vec![
                    Expr::Function(id),
                    Expr::Member(
                        Box::new(ctor.clone()),
                        Box::new(Expr::Value(json!("prototype"))),
                    ),
                ],
            ),
        );
        let hidden = format!("{prefix}initialize");
        self.emit(class.span, Op::Declare(hidden.clone(), init));
        self.snapshot(
            class.span,
            Expr::NodeBuiltin(
                "$$classInitializer".into(),
                vec![ctor.clone(), Expr::Variable(hidden)],
            ),
        );
        let id = self.compile_class_elements(class.span, "static-elements", statics)?;
        let init = self.snapshot(
            class.span,
            Expr::NodeBuiltin(
                "$$methodHome".into(),
                vec![Expr::Function(id), ctor.clone()],
            ),
        );
        self.emit(
            class.span,
            Op::Effect(
                None,
                Effect::Invoke(Box::new(Invocation {
                    callee: init,
                    receiver: ctor.clone(),
                    args: Expr::Array(vec![]),
                    label: None,
                })),
            ),
        );
        self.private_names = saved_private;
        self.leave_scope(class.span, entered);
        Ok(ctor)
    }

    fn class_element_key(&mut self, key: &ast::PropName, hidden: &str) -> Result<Expr> {
        let value = match key {
            ast::PropName::Computed(c) => {
                let value = self.lower(&c.expr)?;
                self.class_helper(c.span, "$$classKey", vec![value])
            }
            key => Expr::Value(json!(self.property(key)?)),
        };
        self.emit(key.span(), Op::Declare(hidden.into(), value));
        Ok(Expr::Variable(hidden.into()))
    }
    fn compile_class_elements(
        &mut self,
        span: Span,
        name: &str,
        elements: Vec<ClassElement<'_>>,
    ) -> Result<String> {
        self.compile_function(FunctionSource {
            name: Some(format!("{name}:{}", span.lo.0)),
            self_name: None,
            params: vec![],
            body: FunctionBody::ClassElements(elements),
            is_async: false,
            is_generator: false,
            lexical_this: false,
            constructable: false,
            span,
        })
    }
    pub(super) fn emit_class_elements(&mut self, elements: Vec<ClassElement<'_>>) -> Result<()> {
        for element in elements {
            match element {
                ClassElement::Field {
                    span,
                    key,
                    hint,
                    value,
                    private,
                } => {
                    let value = if let Some(value) = value {
                        self.lower_named_key(value, Some(hint))?
                    } else {
                        Expr::Undefined
                    };
                    let mut args = vec![Expr::Variable("\0this".into()), key, value];
                    if private {
                        args.extend([Expr::Value(json!("field")), Expr::Undefined]);
                    }
                    self.snapshot(
                        span,
                        Expr::NodeBuiltin(
                            if private {
                                "$$privateAdd"
                            } else {
                                "$$createData"
                            }
                            .into(),
                            args,
                        ),
                    );
                }
                ClassElement::Private {
                    span,
                    key,
                    kind,
                    value,
                    setter,
                } => {
                    self.snapshot(
                        span,
                        Expr::NodeBuiltin(
                            "$$privateAdd".into(),
                            vec![
                                Expr::Variable("\0this".into()),
                                key,
                                value,
                                Expr::Value(json!(kind)),
                                setter,
                            ],
                        ),
                    );
                }
                ClassElement::Block { span, function } => {
                    self.emit(
                        span,
                        Op::Effect(
                            None,
                            Effect::Invoke(Box::new(Invocation {
                                callee: function,
                                receiver: Expr::Variable("\0this".into()),
                                args: Expr::Array(vec![]),
                                label: None,
                            })),
                        ),
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn super_reference(&mut self, p: &ast::SuperPropExpr) -> Result<(Expr, Expr, Expr)> {
        // GetThisBinding precedes even evaluating a computed key. GetSuperBase
        // precedes coercing that key (which can mutate the home prototype).
        let receiver = self.snapshot(p.span, Expr::Variable("\0this".into()));
        let key = match &p.prop {
            ast::SuperProp::Ident(i) => Expr::Value(json!(i.sym.to_string())),
            ast::SuperProp::Computed(c) => {
                let value = self.lower(&c.expr)?;
                self.snapshot(c.span, value)
            }
        };
        let base = self.snapshot(
            p.span,
            Expr::NodeBuiltin("$$superBase".into(), vec![Expr::Variable("\0home".into())]),
        );
        Ok((base, key, receiver))
    }
    pub(super) fn super_get(&mut self, p: &ast::SuperPropExpr) -> Result<Expr> {
        let (base, key, receiver) = self.super_reference(p)?;
        Ok(self.snapshot(
            p.span,
            Expr::NodeBuiltin("$$getProperty".into(), vec![base, key, receiver]),
        ))
    }
    pub(super) fn super_call(&mut self, c: &ast::CallExpr) -> Result<Expr> {
        let parent = self.snapshot(
            c.span,
            Expr::NodeBuiltin("$$superConstructor".into(), vec![]),
        );
        let a = ast::Expr::Array(ast::ArrayLit {
            span: c.span,
            elems: c.args.iter().cloned().map(Some).collect(),
        });
        let args = self.lower(&a)?;
        Ok(self.snapshot(
            c.span,
            Expr::NodeBuiltin("$$superCallArray".into(), vec![args, parent]),
        ))
    }
    pub(super) fn super_assignment(
        &mut self,
        a: &ast::AssignExpr,
        p: &ast::SuperPropExpr,
    ) -> Result<Expr> {
        let (base, key, receiver) = self.super_reference(p)?;
        let op = Self::compound_operator(a.op);
        let old = if a.op != ast::AssignOp::Assign {
            Some(self.snapshot(
                p.span,
                Expr::NodeBuiltin(
                    "$$getProperty".into(),
                    vec![base.clone(), key.clone(), receiver.clone()],
                ),
            ))
        } else {
            None
        };
        let logical = matches!(
            a.op,
            ast::AssignOp::AndAssign | ast::AssignOp::OrAssign | ast::AssignOp::NullishAssign
        );
        let result = self.temp();
        let skip = if logical {
            let old = old.clone().unwrap();
            self.emit(a.span, Op::Set(result.clone(), old.clone()));
            Some(self.emit(
                a.span,
                Op::Branch(Self::logical_assignment_test(a.op, old), 0),
            ))
        } else {
            None
        };
        let value = self.lower(&a.right)?;
        let value = if let (Some(op), Some(old)) = (op, old) {
            Expr::Binary(op.into(), Box::new(old), Box::new(value))
        } else {
            value
        };
        let value = self.snapshot(a.span, value);
        self.class_helper(
            a.span,
            "$$superSet",
            vec![base, key, value.clone(), receiver],
        );
        self.emit(a.span, Op::Set(result.clone(), value));
        if let Some(skip) = skip {
            self.land(skip);
        }
        Ok(Expr::Variable(result))
    }
    pub(super) fn super_update(
        &mut self,
        u: &ast::UpdateExpr,
        p: &ast::SuperPropExpr,
    ) -> Result<Expr> {
        let (base, key, receiver) = self.super_reference(p)?;
        let old = self.snapshot(
            p.span,
            Expr::Unary(
                "+".into(),
                Box::new(Expr::NodeBuiltin(
                    "$$getProperty".into(),
                    vec![base.clone(), key.clone(), receiver.clone()],
                )),
            ),
        );
        let op = if u.op == ast::UpdateOp::PlusPlus {
            "+"
        } else {
            "-"
        };
        let value = self.snapshot(
            u.span,
            Expr::Binary(
                op.into(),
                Box::new(old.clone()),
                Box::new(Expr::Value(json!(1))),
            ),
        );
        self.class_helper(
            u.span,
            "$$superSet",
            vec![base, key, value.clone(), receiver],
        );
        Ok(if u.prefix { value } else { old })
    }
}
