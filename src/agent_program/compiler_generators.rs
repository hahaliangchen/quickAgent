//! Yield keeps its ordinary continuation plus an abrupt-return continuation.
//! Return uses the same finally lowering as a source-level return statement.
use super::*;
use swc_ecma_visit::{VisitMut, VisitMutWith};

impl Compiler {
    pub(super) fn await_value(&mut self, value: Expr, span: Span) -> Expr {
        let destination = self.temp();
        self.emit(
            span,
            Op::Effect(
                Some(destination.clone()),
                Effect::Call("$$awaitPromise".into(), vec![value]),
            ),
        );
        self.emit(
            span,
            Op::Await {
                value: Expr::Variable(destination.clone()),
                destination: destination.clone(),
            },
        );
        Expr::Variable(destination)
    }
    pub(super) fn generator_yield(
        &mut self,
        value: Expr,
        span: Span,
        delegated: bool,
    ) -> Result<Expr> {
        let value = if self.async_generator && !delegated {
            self.await_value(value, span)
        } else {
            value
        };
        let destination = self.temp();
        let instruction = self.emit(
            span,
            Op::Yield {
                value,
                destination: destination.clone(),
                return_target: 0,
                delegated,
            },
        );
        if !delegated {
            let skip = self.emit(span, Op::Jump(0));
            let return_target = self.code.len();
            let value = Expr::Variable(destination.clone());
            let value = if self.async_generator {
                self.await_value(value, span)
            } else {
                value
            };
            let value = self.snapshot(span, value);
            self.emit_finalizers(None, span)?;
            self.emit(span, Op::Return(value));
            self.code[skip].op = Op::Jump(self.code.len());
            if let Op::Yield {
                return_target: target,
                ..
            } = &mut self.code[instruction].op
            {
                *target = return_target;
            }
        }
        Ok(Expr::Variable(destination))
    }

    pub(super) fn lower_yield(&mut self, node: &ast::YieldExpr) -> Result<Expr> {
        let value = node
            .arg
            .as_deref()
            .map(|e| self.lower(e))
            .transpose()?
            .unwrap_or(Expr::Undefined);
        if !node.delegate {
            return self.generator_yield(value, node.span, false);
        }
        // The delegated protocol is normal VM code: getters, next/return/throw,
        // side effects and nested generator resumes can all suspend durably.
        let source = self.temp();
        self.emit(node.span, Op::Set(source.clone(), value));
        let result = self.temp();
        self.temp_names.insert(source.clone());
        self.temp_names.insert(result.clone());
        let prefix = format!("$$generator{}", self.temp());
        let mut text = format!(
            r#"{{
          const {prefix}rec=$$getIterator({source});
          const {prefix}it={prefix}rec.iterator;
          let {prefix}received={{kind:'next',value:undefined}};
          while(true){{
            let {prefix}step;
            if({prefix}received.kind==='next'){{
              {prefix}step=$$invokeWithReceiver({prefix}rec.next,{prefix}it,[{prefix}received.value]);
            }}else{{
              const {prefix}method={prefix}it[{prefix}received.kind];
              if({prefix}method===undefined||{prefix}method===null){{
                if({prefix}received.kind==='return')return {prefix}received.value;
                const {prefix}close={prefix}it.return;
                if({prefix}close!==undefined&&{prefix}close!==null){{
                  const {prefix}closed=$$invokeWithReceiver({prefix}close,{prefix}it,[]);
                  if(!$$isObject({prefix}closed))throw new TypeError('Iterator return must return an object');
                }}
                throw new TypeError('Delegated iterator has no throw method');
              }}
              {prefix}step=$$invokeWithReceiver({prefix}method,{prefix}it,[{prefix}received.value]);
            }}
            if(!$$isObject({prefix}step))throw new TypeError('Iterator result must be an object');
            if({prefix}step.done){{
              const {prefix}value={prefix}step.value;
              if({prefix}received.kind==='return')return {prefix}value;
              {result}={prefix}value;break;
            }}
            {prefix}received=$$generatorYieldResult({prefix}step);
          }}
        }}"#
        );
        if self.async_generator {
            text = text.replace("$$getIterator(", "$$getAsyncIterator(");
            text = text.replace("=$$invokeWithReceiver(", "=await $$invokeWithReceiver(");
            text = text.replace(
                &format!("$$generatorYieldResult({prefix}step)"),
                &format!("$$generatorYieldResult({prefix}step.value)"),
            );
            text=text.replace(&format!("let {prefix}step;"),&format!("if({prefix}received.kind==='return'){prefix}received.value=await {prefix}received.value;let {prefix}step;"));
        }
        let file = self.map.new_source_file(
            FileName::Custom("<generator delegation>".into()).into(),
            text,
        );
        let lexer = Lexer::new(
            Syntax::Es(EsSyntax::default()),
            Default::default(),
            StringInput::from(&*file),
            None,
        );
        let mut statement = Parser::new_from(lexer)
            .parse_stmt()
            .map_err(|e| anyhow::anyhow!("internal yield* lowering: {:?}", e.kind()))?;
        struct Locations(Span);
        impl VisitMut for Locations {
            fn visit_mut_span(&mut self, span: &mut Span) {
                *span = self.0;
            }
        }
        statement.visit_mut_with(&mut Locations(node.span));
        self.stmt(&statement)?;
        Ok(Expr::Variable(result))
    }
}
