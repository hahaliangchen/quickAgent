//! Classes lower to ordinary durable closures. Home objects and constructor
//! initialization live in the heap/runtime, never in a host closure.
use super::*;

impl Compiler {
    fn class_helper(&mut self, span: Span, name: &str, args: Vec<Expr>) -> Expr {
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
        let entered =
            self.enter_scope(class.span, name.iter().map(|n| (n.clone(), true)).collect())?;
        let parent = if let Some(parent) = &class.super_class {
            let value = self.lower(parent)?;
            self.snapshot(parent.span(), value)
        } else {
            Expr::Undefined
        };
        let derived = class.super_class.is_some();
        let explicit = class.body.iter().find_map(|m| {
            if let ast::ClassMember::Constructor(c) = m {
                Some(c)
            } else {
                None
            }
        });
        // Parsing a small generated function shares the normal parameter/body
        // compiler, including defaults, rest, try/finally and suspension.
        let default;
        let source = if let Some(c) = explicit {
            let params = c
                .params
                .iter()
                .map(|p| match p {
                    ast::ParamOrTsParamProp::Param(p) => Ok(&p.pat),
                    _ => self.error(c.span, "TypeScript parameter properties are unsupported"),
                })
                .collect::<Result<Vec<_>>>()?;
            FunctionSource {
                name: name.clone(),
                self_name: None,
                params,
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
        for member in &class.body {
            match member {
                ast::ClassMember::Constructor(_) | ast::ClassMember::Empty(_) => {},
                ast::ClassMember::Method(m) => {
                    let key=match &m.key {
                        ast::PropName::Computed(c)=>{let value=self.lower(&c.expr)?;self.class_helper(c.span,"$$classKey",vec![value])},
                        p=>Expr::Value(json!(self.property(p)?)),
                    };
                    let hint=if let Expr::Value(Value::String(s))=&key{s.clone()}else{String::new()};
                    let function=self.compile_function(FunctionSource::method(&m.function,Some(hint)))?;
                    let kind=match m.kind {ast::MethodKind::Method=>"value",ast::MethodKind::Getter=>"get",ast::MethodKind::Setter=>"set"};
                    self.snapshot(m.span,Expr::NodeBuiltin("$$classMethod".into(),vec![ctor.clone(),key,Expr::Function(function),Expr::Value(json!(kind)),Expr::Value(json!(m.is_static))]));
                },
                _=>return self.error(member.span(),"class fields, private elements and static blocks are not implemented; use constructors and methods"),
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
        self.leave_scope(class.span, entered);
        Ok(ctor)
    }

    fn super_reference(&mut self, p: &ast::SuperPropExpr) -> Result<(Expr, Expr, Expr)> {
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
