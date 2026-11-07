//! Iteration uses ordinary resumable calls and the existing finally machinery.
use super::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

impl Compiler {
    pub(super) fn iterable_for_of(&mut self, f: &ast::ForOfStmt) -> Result<ast::Stmt> {
        if f.is_await {
            return self.error(f.span,"for await is not available yet; Symbol.asyncIterator exists but async iteration is a later stage");
        }
        self.loop_head(&f.left, f.span)?;
        let suffix = self.temp();
        let rec = format!("$$forOfRecord{suffix}");
        let step = format!("$$forOfStep{suffix}");
        let error = format!("$$forOfError{suffix}");
        let text = format!(
            "{{const {rec}=$$getIterator(__SOURCE);try{{for(;;){{const {step}=$$iteratorStep({rec});if({step}.done)break;__BIND=__VALUE;__BODY;}}}}catch({error}){{$$iteratorCloseThrow({rec},{error});}}finally{{$$iteratorCloseNormal({rec});}}}}"
        );
        let file = self
            .map
            .new_source_file(FileName::Custom("<iterator lowering>".into()).into(), text);
        let lexer = Lexer::new(
            Syntax::Es(EsSyntax::default()),
            Default::default(),
            StringInput::from(&*file),
            None,
        );
        let mut stmt = Parser::new_from(lexer)
            .parse_stmt()
            .map_err(|e| anyhow::anyhow!("internal iterator lowering: {:?}", e.kind()))?;
        struct Rewrite<'a> {
            f: &'a ast::ForOfStmt,
            step: String,
        }
        impl VisitMut for Rewrite<'_> {
            fn visit_mut_span(&mut self, span: &mut Span) {
                *span = self.f.span;
            }
            fn visit_mut_expr(&mut self, e: &mut ast::Expr) {
                if matches!(e,ast::Expr::Ident(i) if i.sym==*"__SOURCE") {
                    *e = (*self.f.right).clone();
                    return;
                }
                if matches!(e,ast::Expr::Ident(i) if i.sym==*"__VALUE") {
                    *e = ast::Expr::Member(ast::MemberExpr {
                        span: self.f.span,
                        obj: Box::new(ast::Expr::Ident(ast::Ident::new_no_ctxt(
                            self.step.clone().into(),
                            self.f.span,
                        ))),
                        prop: ast::MemberProp::Ident(ast::IdentName::new(
                            "value".into(),
                            self.f.span,
                        )),
                    });
                    return;
                }
                e.visit_mut_children_with(self);
            }
            fn visit_mut_stmt(&mut self, s: &mut ast::Stmt) {
                if matches!(s,ast::Stmt::Expr(e) if matches!(&*e.expr,ast::Expr::Ident(i) if i.sym==*"__BODY"))
                {
                    *s = (*self.f.body).clone();
                    return;
                }
                if let ast::Stmt::Expr(e) = s
                    && let ast::Expr::Assign(a) = &mut *e.expr
                    && matches!(&a.left,ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(i)) if i.id.sym==*"__BIND")
                {
                    a.span = self.f.span;
                    self.visit_mut_expr(&mut a.right);
                    match &self.f.left {
                        ast::ForHead::VarDecl(d) => {
                            let mut d = d.clone();
                            d.decls[0].init = Some(a.right.clone());
                            *s = ast::Stmt::Decl(ast::Decl::Var(d));
                        }
                        ast::ForHead::Pat(p) => {
                            a.left = ast::AssignTarget::try_from(p.clone())
                                .expect("validated for-of assignment pattern");
                        }
                        _ => unreachable!(),
                    }
                    return;
                }
                s.visit_mut_children_with(self);
            }
        }
        stmt.visit_mut_with(&mut Rewrite { f, step });
        Ok(stmt)
    }
}
