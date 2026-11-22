//! Iteration uses ordinary resumable calls and the existing finally machinery.
use super::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};
use swc_common::SourceMapper;

impl Compiler {
    /// SWC 39.1 drops trailing array elisions when converting an expression to
    /// an arrow-parameter/assignment pattern (parser/pat.rs, Expr::Array arm).
    /// Its direct binding parser preserves them. Read only the original span
    /// and recover the missing trailing None entries; keep all original nodes
    /// and locations, including defaults, closures and host contracts.
    pub(super) fn array_binding_len(&self, array: &ast::ArrayPat) -> usize {
        let Ok(source) = self.map.span_to_snippet(array.span) else { return array.elems.len(); };
        if !source.trim_start().starts_with('[') { return array.elems.len(); }
        let map: Lrc<SourceMap> = Default::default();
        let file = map.new_source_file(FileName::Custom("<array binding shape>".into()).into(), source);
        let lexer = Lexer::new(Syntax::Es(EsSyntax::default()), Default::default(), StringInput::from(&*file), None);
        let mut parser = Parser::new_from(lexer);
        parser.set_allow_super_call(true);
        match parser.parse_pat() {
            Ok(ast::Pat::Array(parsed)) => parsed.elems.len().max(array.elems.len()),
            // Synthetic spans and assignment-only targets need no correction
            // here; the original source has already passed the module parser.
            _ => array.elems.len(),
        }
    }
    pub(super) fn iterable_for_of(&mut self, f: &ast::ForOfStmt) -> Result<ast::Stmt> {
        self.loop_head(&f.left, f.span)?;
        let suffix = self.temp();
        let rec = format!("$$forOfRecord{suffix}");
        let step = format!("$$forOfStep{suffix}");
        let error = format!("$$forOfError{suffix}");
        let mut text = format!(
            "{{const {rec}=$$getIterator(__SOURCE);try{{for(;;){{const {step}=$$iteratorStep({rec});if({step}.done)break;__BIND=__VALUE;__BODY;}}}}catch({error}){{$$iteratorCloseThrow({rec},{error});}}finally{{$$iteratorCloseNormal({rec});}}}}"
        );
        if f.is_await {
            let result=format!("$$forOfResult{suffix}");
            let close=format!("$$forOfClose{suffix}");
            let closed=format!("$$forOfClosed{suffix}");
            let cleanup=format!("if(!{rec}.done){{{rec}.done=true;const {close}={rec}.iterator.return;if({close}!==undefined&&{close}!==null){{const {closed}=await $$invokeWithReceiver({close},{rec}.iterator,[]);if(!$$isObject({closed}))throw new TypeError('Iterator return must return an object');}}}}");
            // Await only the protocol's next/return calls. An async helper
            // around the loop would insert observable extra microtask turns.
            text=format!("{{const {rec}=$$getAsyncIterator(__SOURCE);try{{for(;;){{{rec}.done=true;const {result}=await $$invokeWithReceiver({rec}.next,{rec}.iterator,[]);if(!$$isObject({result}))throw new TypeError('Iterator result must be an object');if({result}.done)break;const {step}={{value:{result}.value}};{rec}.done=false;__BIND=__VALUE;__BODY;}}}}catch({error}){{try{{{cleanup}}}catch($$ignored{suffix}){{}}throw {error};}}finally{{{cleanup}}}}}");
        }
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
