//! Lift property reads into durable calls, including reads introduced by
//! destructuring and method-reference lowering. No getter runs in eval().
use super::*;
impl Compiler {
    pub(super) fn raw_emit(&mut self, span: Span, op: Op) -> usize {
        let n = self.code.len();
        self.code.push(Instruction {
            at: self.at(span),
            op,
        });
        n
    }
    fn property_expr(&mut self, span: Span, e: &mut Expr) {
        match e {
            Expr::Conditional(test, yes, no) => {
                self.property_expr(span, test);
                let dest = self.temp();
                let branch = self.raw_emit(span, Op::Branch(*test.clone(), 0));
                self.property_expr(span, yes);
                self.raw_emit(span, Op::Set(dest.clone(), *yes.clone()));
                let end = self.raw_emit(span, Op::Jump(0));
                let otherwise = self.code.len();
                self.code[branch].op = Op::Branch(*test.clone(), otherwise);
                self.property_expr(span, no);
                self.raw_emit(span, Op::Set(dest.clone(), *no.clone()));
                let stop = self.code.len();
                self.code[end].op = Op::Jump(stop);
                *e = Expr::Variable(dest);
                return;
            }
            Expr::Binary(op, a, b) if matches!(op.as_str(), "&&" | "||" | "??") => {
                // The left side is evaluated once even when it is an accessor.
                let mut left = *a.clone();
                self.property_expr(span, &mut left);
                let saved = self.temp();
                self.raw_emit(span, Op::Set(saved.clone(), left));
                let yes = b.clone();
                let test = match op.as_str() {
                    "&&" => Expr::Variable(saved.clone()),
                    "||" => Expr::Unary("!".into(), Box::new(Expr::Variable(saved.clone()))),
                    _ => Expr::Binary(
                        "==".into(),
                        Box::new(Expr::Variable(saved.clone())),
                        Box::new(Expr::Value(Value::Null)),
                    ),
                };
                *e = Expr::Conditional(Box::new(test), yes, Box::new(Expr::Variable(saved)));
                self.property_expr(span, e);
                return;
            }
            Expr::Member(a, b) | Expr::Binary(_, a, b) => {
                self.property_expr(span, a);
                self.property_expr(span, b);
            }
            Expr::Unary(_, a) | Expr::Stringify(a) | Expr::ParseJson(a) => {
                self.property_expr(span, a)
            }
            Expr::Array(xs) | Expr::Template(xs) | Expr::NodeBuiltin(_, xs) | Expr::RegExp(xs) => {
                for x in xs {
                    self.property_expr(span, x);
                }
            }
            Expr::Object(xs) => {
                for (_, x) in xs {
                    self.property_expr(span, x);
                }
            }
            Expr::StringMethod(receiver, _, xs) => {
                self.property_expr(span, receiver);
                for x in xs {
                    self.property_expr(span, x);
                }
            }
            _ => {}
        }
        match e {
            Expr::Binary(op, a, b)
                if matches!(
                    op.as_str(),
                    "+" | "-"
                        | "*"
                        | "/"
                        | "%"
                        | "**"
                        | "<"
                        | "<="
                        | ">"
                        | ">="
                        | "&"
                        | "|"
                        | "^"
                        | "<<"
                        | ">>"
                        | ">>>"
                ) =>
            {
                let hint = if op == "+" { "default" } else { "number" };
                self.coerce_primitive(span, a, hint);
                self.coerce_primitive(span, b, hint);
            }
            Expr::Unary(op, a) if matches!(op.as_str(), "+" | "-" | "~") => {
                self.coerce_primitive(span, a, "number")
            }
            Expr::Template(xs) => {
                for x in xs {
                    self.coerce_primitive(span, x, "string");
                }
            }
            _ => {}
        }
        let call = match e {
            Expr::Binary(op, a, b) if op == "in" => Some(Effect::Call(
                "$$inOperator".into(),
                vec![*a.clone(), *b.clone()],
            )),
            Expr::Binary(op, a, b) if matches!(op.as_str(), "==" | "!=") => Some(Effect::Dynamic {
                callee: Expr::Native(
                    if op == "==" {
                        "$$looseEqual"
                    } else {
                        "$$looseNotEqual"
                    }
                    .into(),
                ),
                args: vec![*a.clone(), *b.clone()],
            }),
            Expr::Binary(op, a, b) if op == "instanceof" => Some(Effect::Call(
                "$$instanceOf".into(),
                vec![*a.clone(), *b.clone()],
            )),
            Expr::Member(object, key) => Some(Effect::Dynamic {
                callee: Expr::Native("$$getProperty".into()),
                args: vec![*object.clone(), *key.clone()],
            }),
            Expr::NodeBuiltin(name, args)
                if matches!(
                    name.as_str(),
                    "$$getProperty"
                        | "$$constructWithTarget"
                        | "$$invokeWithReceiver"
                        | "$$awaitSubscribe"
                        | "$$makeBound"
                        | "$$superCallArray"
                        | "$$superConstructor"
                        | "$$constructReady"
                ) =>
            {
                Some(Effect::Dynamic {
                    callee: Expr::Native(name.clone()),
                    args: args.clone(),
                })
            }
            Expr::NodeBuiltin(name, args) => super::super::js_functions::resumable_builtin(name)
                .map(|helper| Effect::Call(helper.into(), args.clone())),
            _ => None,
        };
        if let Some(effect) = call {
            let dest = self.temp();
            self.raw_emit(span, Op::Effect(Some(dest.clone()), effect));
            *e = Expr::Variable(dest);
        }
    }
    fn coerce_primitive(&mut self, span: Span, e: &mut Expr, hint: &str) {
        if matches!(e, Expr::Undefined | Expr::Value(_) | Expr::Utf16(_)) {
            return;
        }
        let dest = self.temp();
        self.raw_emit(
            span,
            Op::Effect(
                Some(dest.clone()),
                Effect::Dynamic {
                    callee: Expr::Native("$$coercePrimitive".into()),
                    args: vec![e.clone(), Expr::Value(json!(hint))],
                },
            ),
        );
        *e = Expr::Variable(dest);
    }
    pub(super) fn property_op(&mut self, span: Span, op: &mut Op) {
        let mut visit = |e: &mut Expr| self.property_expr(span, e);
        match op {
            Op::Set(_, e)
            | Op::Declare(_, e)
            | Op::Return(e)
            | Op::Throw(e)
            | Op::Branch(e, _)
            | Op::Await { value: e, .. }
            | Op::Yield { value: e, .. } => visit(e),
            Op::SetMember { object, key, value } => {
                visit(object);
                visit(key);
                visit(value);
            }
            Op::Append { items, .. } => {
                for e in items {
                    visit(e);
                }
            }
            Op::Effect(_, effect) => match effect {
                Effect::Invoke(c) | Effect::Construct(c) => {
                    visit(&mut c.callee);
                    visit(&mut c.receiver);
                    visit(&mut c.args);
                    if let Some(l) = &mut c.label {
                        visit(l);
                    }
                }
                Effect::PromiseCreate { value, .. }
                | Effect::PromiseConstruct { executor: value } => visit(value),
                Effect::Call(_, xs)
                | Effect::Fork(_, xs)
                | Effect::Tool(_, xs)
                | Effect::Join(xs) => {
                    for e in xs {
                        visit(e);
                    }
                }
                Effect::Dynamic { callee, args } => {
                    visit(callee);
                    for e in args {
                        visit(e);
                    }
                }
                Effect::Apply { callee, args } => {
                    visit(callee);
                    visit(args);
                }
                Effect::Method { receiver, args, .. } => {
                    visit(receiver);
                    for e in args {
                        visit(e);
                    }
                }
                Effect::MethodApply { receiver, args, .. } => {
                    visit(receiver);
                    visit(args);
                }
                Effect::DynamicMethodApply {
                    receiver,
                    name,
                    args,
                } => {
                    visit(receiver);
                    visit(name);
                    visit(args);
                }
                Effect::Task { input, .. } => visit(input),
                Effect::ParallelReads(calls) => {
                    for (_, args) in calls {
                        for e in args {
                            visit(e);
                        }
                    }
                }
                Effect::ParallelReadMap { items, args, .. } => {
                    visit(items);
                    for e in args {
                        visit(e);
                    }
                }
                Effect::Require(_) => {}
            },
            _ => {}
        }
    }
}
