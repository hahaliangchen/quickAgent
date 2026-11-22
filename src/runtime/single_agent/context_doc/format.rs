//! Value formatter for Context-as-Code v2 pure Append-Only `// =>` receipts.
//!
//! - Results `<= 64KB` (`INLINE_LIMIT_CHARS`) are printed inline permanently in `// =>`
//!   (within a per-step 64KB inline budget) so the model never suffers 1-step amnesia.
//! - Results `> 64KB` stay live in VM memory as `ResourceHandle` (`Ref#N`) with a
//!   compact permanent signature + high-entropy structural/error index so the model
//!   can page/slice any section via `.slice()`, `.grep()`, `.outline()`, `.head()`, `.tail()`.
//! - Handles passed to `@virtual` in the same step (Pass-by-Handle) keep compact
//!   signatures in the outer trace and auto-dereference cleanly inside the child frame.

use super::{INLINE_LIMIT_CHARS, ShownFileHandle};
use super::indexers::{
    add_line_numbers, describe_item_shape, format_comment_block, format_large_array_index,
    format_large_log_index, format_large_source_index,
};
use crate::runtime::agent_program::Runtime;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use swc_common::{FileName, SourceMap, sync::Lrc};
use swc_ecma_ast as ast;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, lexer::Lexer};
use swc_ecma_visit::{Visit, VisitWith};

/// Minimum length at which a duplicate inline binding in the same step folds
/// into `(same as <earlierVar>)` to avoid wasting permanent trace tokens.
const INLINE_DEDUP_MIN_CHARS: usize = 120;

/// Minimum length at which command outputs receive a `Ref#N` handle tag so
/// `.slice()` / `.grep()` / `.tail()` are available on medium & large logs.
const COMMAND_HANDLE_MIN_CHARS: usize = 4_000;

/// Strict hard cap on `console.log` output so accidental loops or bulk `console.log`
/// calls can NEVER blow up context. Models should leave bare expressions or sink
/// variables instead of calling `console.log`.
const CONSOLE_LIMIT_CHARS: usize = 4_096;

/// Context hints collected during one VM segment execution:
/// - `virtual_receipts`: factual `[virtual ...]` / `[library ...]` summary lines.
/// - `delegated_refs` / `delegated_content_hashes`: Resource Handles (and large text
///   payloads) passed into `@virtual` calls in the SAME step, so the outer parent skips
///   expanding their raw bodies (Pass-by-Handle zero outer bloat).
#[derive(Clone, Debug, Default)]
pub struct SegmentContextHints {
    pub virtual_receipts: Vec<String>,
    pub delegated_refs: BTreeSet<String>,
    pub delegated_content_hashes: BTreeSet<u64>,
}

/// Fast deterministic FNV-1a 64-bit hash used for content deduplication & handle matching.
pub(in crate::runtime::single_agent) fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Attach a monotonic `"__ref": "Ref#N"` handle tag to file-read results and
/// medium/large command outputs so they act as first-class Lazy Resource Handles.
pub(in crate::runtime::single_agent) fn tag_resource_handle(value: &mut Value, next_ref_id: &mut usize) {
    let mut empty = BTreeMap::new();
    tag_resource_handle_in_turn(value, next_ref_id, &mut empty);
}

/// Like [`tag_resource_handle`], but reuses an existing `Ref#N` handle identity
/// when an unchanged file `(path, content_hash)` is re-read in the same turn.
pub(in crate::runtime::single_agent) fn tag_resource_handle_in_turn(
    value: &mut Value,
    next_ref_id: &mut usize,
    file_refs: &mut BTreeMap<String, (u64, String)>,
) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if obj.contains_key("__ref") {
        return;
    }
    if let (Some(path), Some(content)) = (
        obj.get("path").and_then(Value::as_str),
        obj.get("content").and_then(Value::as_str),
    ) {
        let hash = fnv1a64(content);
        if let Some((prev_hash, prev_ref)) = file_refs.get(path)
            && *prev_hash == hash
        {
            obj.insert("__ref".into(), Value::String(prev_ref.clone()));
            return;
        }
        let id = *next_ref_id;
        *next_ref_id += 1;
        let ref_id = format!("Ref#{id}");
        file_refs.insert(path.to_owned(), (hash, ref_id.clone()));
        obj.insert("__ref".into(), Value::String(ref_id));
        return;
    }
    let is_large_cmd = obj.contains_key("exit_code")
        && (obj
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .count()
            + obj
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .count()
            > COMMAND_HANDLE_MIN_CHARS);
    if is_large_cmd {
        let id = *next_ref_id;
        *next_ref_id += 1;
        obj.insert("__ref".into(), Value::String(format!("Ref#{id}")));
    }
}

/// Recursively collect any `"__ref"` handle IDs and large string hashes
/// passed into a `@virtual` call (via args or `@uses` captures).
pub(in crate::runtime::single_agent) fn collect_delegated_markers(
    value: &Value,
    refs: &mut BTreeSet<String>,
    hashes: &mut BTreeSet<u64>,
) {
    match value {
        Value::String(s) => {
            if s.chars().count() > 2_000 {
                hashes.insert(fnv1a64(s));
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_delegated_markers(item, refs, hashes);
            }
        }
        Value::Object(map) => {
            if let Some(r) = map.get("__ref").and_then(Value::as_str) {
                refs.insert(r.to_owned());
            }
            for v in map.values() {
                collect_delegated_markers(v, refs, hashes);
            }
        }
        _ => {}
    }
}

/// Outcome of checking whether a file handle should expand its body/index in this step.
enum FileDisplayState {
    /// First time this `(path, content_hash)` is expanded in the turn's visible wire history.
    Show,
    /// Delegated to `@virtual` or already expanded earlier in this exact same step.
    SameStepOrDelegated,
    /// Unchanged file already expanded in an earlier still-visible step of this turn.
    UnchangedInTurn { prior_ref: String, prior_step: usize },
}

/// Per-step display tracker: enforces the 64KB per-step inline budget,
/// deduplicates aliases within a step, reuses unchanged file handles across
/// steps in the same turn, and honors Pass-by-Handle delegation.
pub(super) struct DisplayTracker<'a> {
    delegated_refs: Option<&'a BTreeSet<String>>,
    delegated_hashes: Option<&'a BTreeSet<u64>>,
    seen_refs: BTreeSet<String>,
    seen_hashes: BTreeSet<u64>,
    prior_shown_files: Option<&'a mut BTreeMap<String, ShownFileHandle>>,
    segment_index: usize,
    remaining_inline_budget: usize,
    is_virtual_param: bool,
}

impl<'a> DisplayTracker<'a> {
    pub(super) fn from_hints(
        hints: &'a SegmentContextHints,
        inline_budget_chars: usize,
        prior_shown_files: Option<&'a mut BTreeMap<String, ShownFileHandle>>,
        segment_index: usize,
    ) -> Self {
        Self {
            delegated_refs: Some(&hints.delegated_refs),
            delegated_hashes: Some(&hints.delegated_content_hashes),
            seen_refs: BTreeSet::new(),
            seen_hashes: BTreeSet::new(),
            prior_shown_files,
            segment_index,
            remaining_inline_budget: inline_budget_chars.min(INLINE_LIMIT_CHARS),
            is_virtual_param: false,
        }
    }

    pub(super) fn for_virtual_param() -> Self {
        Self {
            delegated_refs: None,
            delegated_hashes: None,
            seen_refs: BTreeSet::new(),
            seen_hashes: BTreeSet::new(),
            prior_shown_files: None,
            segment_index: 0,
            remaining_inline_budget: INLINE_LIMIT_CHARS,
            is_virtual_param: true,
        }
    }

    fn is_delegated(&self, ref_id: Option<&str>, text_hash: Option<u64>) -> bool {
        if let Some(r) = ref_id
            && self.delegated_refs.is_some_and(|set| set.contains(r))
        {
            return true;
        }
        if let Some(h) = text_hash
            && self.delegated_hashes.is_some_and(|set| set.contains(&h))
        {
            return true;
        }
        false
    }

    /// Returns `true` if this resource has NOT been delegated or already expanded in this step.
    fn claim_display(&mut self, ref_id: Option<&str>, text_hash: Option<u64>) -> bool {
        if self.is_delegated(ref_id, text_hash) {
            return false;
        }
        if let Some(r) = ref_id
            && !self.seen_refs.insert(r.to_owned())
        {
            return false;
        }
        if let Some(h) = text_hash
            && !self.seen_hashes.insert(h)
        {
            return false;
        }
        true
    }

    /// Check whether a file handle should expand, stay compact (same-step/delegated),
    /// or emit a compact turn-scoped reuse receipt because an identical version was
    /// already expanded in an earlier still-visible step of this turn.
    fn claim_file_display(
        &mut self,
        path: &str,
        ref_id: Option<&str>,
        content_hash: u64,
    ) -> FileDisplayState {
        if !self.claim_display(ref_id, Some(content_hash)) {
            return FileDisplayState::SameStepOrDelegated;
        }
        if !path.is_empty()
            && let Some(prior) = self
                .prior_shown_files
                .as_deref()
                .and_then(|map| map.get(path))
            && prior.content_hash == content_hash
        {
            return FileDisplayState::UnchangedInTurn {
                prior_ref: prior.ref_id.clone(),
                prior_step: prior.segment_index + 1,
            };
        }
        FileDisplayState::Show
    }

    /// Record that `(path, content_hash)` actually had its inline body or structural
    /// index expanded in this segment so later unchanged re-reads stay compact.
    fn record_file_shown(&mut self, path: &str, ref_id: Option<&str>, content_hash: u64) {
        if path.is_empty() {
            return;
        }
        if let Some(map) = self.prior_shown_files.as_deref_mut() {
            map.insert(
                path.to_owned(),
                ShownFileHandle {
                    ref_id: ref_id.unwrap_or("").to_owned(),
                    content_hash,
                    segment_index: self.segment_index,
                },
            );
        }
    }

    fn take_inline_budget(&mut self, chars: usize) -> bool {
        if chars <= self.remaining_inline_budget {
            self.remaining_inline_budget -= chars;
            true
        } else {
            false
        }
    }

    /// Returns `true` if `value` contains at least one resource (`__ref` or large text)
    /// and EVERY such resource inside it was already delegated to `@virtual` or already
    /// expanded earlier in this step.
    fn container_already_covered(&self, value: &Value) -> bool {
        fn visit(v: &Value, t: &DisplayTracker<'_>, found_any: &mut bool, all_covered: &mut bool) {
            if !*all_covered {
                return;
            }
            match v {
                Value::String(s) if s.chars().count() > 2_000 => {
                    *found_any = true;
                    let h = fnv1a64(s);
                    let covered = t.is_delegated(None, Some(h)) || t.seen_hashes.contains(&h);
                    if !covered {
                        *all_covered = false;
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        visit(item, t, found_any, all_covered);
                    }
                }
                Value::Object(map) => {
                    if let Some(r) = map.get("__ref").and_then(Value::as_str) {
                        *found_any = true;
                        let h = map.get("content").and_then(Value::as_str).map(fnv1a64);
                        let covered = t.is_delegated(Some(r), h)
                            || t.seen_refs.contains(r)
                            || h.is_some_and(|hh| t.seen_hashes.contains(&hh));
                        if !covered {
                            *all_covered = false;
                        }
                        return;
                    }
                    for val in map.values() {
                        visit(val, t, found_any, all_covered);
                    }
                }
                _ => {}
            }
        }
        let mut found_any = false;
        let mut all_covered = true;
        visit(value, self, &mut found_any, &mut all_covered);
        found_any && all_covered
    }
}

pub(super) fn format_segment_feedback(
    source: &str,
    runtime: &Runtime,
    pre_fingerprints: &BTreeMap<String, String>,
    console_logs: &[String],
    hints: &SegmentContextHints,
    ignore_trailing_return: bool,
    inline_budget_chars: usize,
    prior_shown_files: Option<&mut BTreeMap<String, ShownFileHandle>>,
    segment_index: usize,
) -> Vec<String> {
    let mut receipts = Vec::new();
    let mut tracker = DisplayTracker::from_hints(
        hints,
        inline_budget_chars,
        prior_shown_files,
        segment_index,
    );

    if !console_logs.is_empty() {
        let joined = console_logs.join("\n");
        let chars = joined.chars().count();
        let h = fnv1a64(&joined);
        if chars <= CONSOLE_LIMIT_CHARS && tracker.take_inline_budget(chars) {
            tracker.seen_hashes.insert(h);
            receipts.push(format_comment_block("console", &joined));
        } else {
            let capped_budget = CONSOLE_LIMIT_CHARS.min(tracker.remaining_inline_budget);
            let head: String = joined.chars().take(capped_budget).collect();
            let _ = tracker.take_inline_budget(head.chars().count());
            let mut body = head;
            body.push_str(&format!(
                "\n... [console truncated at 4KB ({chars} chars total) — do NOT use console.log for bulk output; return bare expressions or sink variables instead]"
            ));
            receipts.push(format_comment_block("console", &body));
        }
    }

    let live_values = runtime.named_bindings();
    let _ = pre_fingerprints;

    // Dataflow Sink rule:
    // 1. Any binding consumed by a later statement in this step is an intermediate
    //    value -> stays silent in live VM memory (zero context clutter).
    // 2. Any binding declared/mutated in this step and NOT consumed later is a
    //    dataflow sink -> display it.
    // 3. All bare non-assignment query expressions (e.g. `ppF.slice(...); shellF.slice(...);`
    //    or `({ a, b })`) are explicit query sinks -> display all of them.
    let sink_names = dataflow_sink_bindings(source, ignore_trailing_return);
    let expr_sinks = runtime.segment_expr_sinks();

    let mut items_to_display: Vec<(String, Value)> = Vec::new();
    for name in sink_names {
        if let Some(val) = live_values.get(&name) {
            items_to_display.push((name, val.clone()));
        }
    }

    if expr_sinks.len() == 1 {
        items_to_display.push(("$_".to_owned(), expr_sinks.into_iter().next().unwrap()));
    } else if expr_sinks.len() > 1 {
        for (idx, val) in expr_sinks.into_iter().enumerate() {
            items_to_display.push((format!("$_{}", idx + 1), val));
        }
    } else if !ignore_trailing_return
        && runtime.segment_returned_explicitly()
        && let Some(val) = live_values.get("$_")
    {
        items_to_display.push(("$_".to_owned(), val.clone()));
    }

    let mut seen_inline: BTreeMap<String, String> = BTreeMap::new();

    for (name, value) in &items_to_display {
        let canonical = canonical_inline_key(value);
        if let Some(key) = &canonical
            && let Some(orig) = seen_inline.get(key)
        {
            receipts.push(format!("// => {name} = (same as {orig})"));
            continue;
        }
        let receipt = format_binding(name, value, &mut tracker);
        if let Some(key) = canonical {
            seen_inline.entry(key).or_insert_with(|| name.clone());
        }
        receipts.push(receipt);
    }

    receipts
}

/// Analyze top-level statement dataflow and return all top-level bindings declared
/// or mutated in `source` that are NOT read/consumed by any later statement in the
/// same segment (preserving order of their final definition/mutation).
fn dataflow_sink_bindings(source: &str, ignore_trailing_return: bool) -> Vec<String> {
    let map: Lrc<SourceMap> = Default::default();
    let file = map.new_source_file(
        FileName::Custom("segment.js".into()).into(),
        source.to_owned(),
    );
    let mut parser = Parser::new_from(Lexer::new(
        Syntax::Es(EsSyntax {
            allow_return_outside_function: true,
            ..Default::default()
        }),
        Default::default(),
        StringInput::from(&*file),
        None,
    ));
    let Ok(module) = parser.parse_module() else {
        return Vec::new();
    };
    let mut stmts: Vec<&ast::Stmt> = module
        .body
        .iter()
        .filter_map(|item| match item {
            ast::ModuleItem::Stmt(stmt) if !matches!(stmt, ast::Stmt::Empty(_)) => Some(stmt),
            _ => None,
        })
        .collect();
    if ignore_trailing_return
        && stmts
            .last()
            .is_some_and(|s| matches!(s, ast::Stmt::Return(_)))
    {
        stmts.pop();
    }
    if stmts.is_empty() {
        return Vec::new();
    }

    // Per-statement: (defined/mutated top-level bindings, identifiers read in this statement).
    let mut stmt_defs: Vec<Vec<String>> = Vec::with_capacity(stmts.len());
    let mut stmt_reads: Vec<BTreeSet<String>> = Vec::with_capacity(stmts.len());

    for stmt in &stmts {
        let mut defs = Vec::new();
        match stmt {
            ast::Stmt::Decl(ast::Decl::Var(var_decl)) => {
                for decl in &var_decl.decls {
                    collect_pat_idents(&decl.name, &mut defs);
                }
            }
            ast::Stmt::Decl(_) | ast::Stmt::Return(_) => {}
            other => {
                let mut collector = MutatedIdentCollector::default();
                other.visit_with(&mut collector);
                defs.extend(collector.names);
            }
        }
        let mut reader = ReadIdentCollector::default();
        stmt.visit_with(&mut reader);
        stmt_defs.push(defs);
        stmt_reads.push(reader.reads);
    }

    // Record the last statement index where each variable was defined/mutated.
    let mut last_def_at: BTreeMap<String, usize> = BTreeMap::new();
    let mut def_order: Vec<(String, usize)> = Vec::new();
    for (idx, defs) in stmt_defs.iter().enumerate() {
        for name in defs {
            last_def_at.insert(name.clone(), idx);
            def_order.push((name.clone(), idx));
        }
    }

    // Suffix union of all identifiers read in statements strictly after `idx`.
    let mut suffix_reads: Vec<BTreeSet<String>> = vec![BTreeSet::new(); stmts.len() + 1];
    for idx in (0..stmts.len()).rev() {
        let mut combined = suffix_reads[idx + 1].clone();
        combined.extend(stmt_reads[idx].iter().cloned());
        suffix_reads[idx] = combined;
    }

    let mut sinks = Vec::new();
    let mut emitted = BTreeSet::new();
    for (name, idx) in def_order {
        if last_def_at.get(&name) != Some(&idx) {
            continue;
        }
        let consumed_later = suffix_reads[idx + 1].contains(&name);
        if !consumed_later && emitted.insert(name.clone()) {
            sinks.push(name);
        }
    }
    sinks
}

#[derive(Default)]
struct ReadIdentCollector {
    reads: BTreeSet<String>,
    shadowed: Vec<BTreeSet<String>>,
}

impl ReadIdentCollector {
    fn is_shadowed(&self, name: &str) -> bool {
        self.shadowed.iter().any(|scope| scope.contains(name))
    }

    fn record_read(&mut self, name: &str) {
        if !self.is_shadowed(name) {
            self.reads.insert(name.to_owned());
        }
    }

    fn visit_pat_expressions(&mut self, pat: &ast::Pat) {
        match pat {
            ast::Pat::Ident(_) => {}
            ast::Pat::Array(arr) => {
                for elem in arr.elems.iter().flatten() {
                    self.visit_pat_expressions(elem);
                }
            }
            ast::Pat::Object(obj) => {
                for prop in &obj.props {
                    match prop {
                        ast::ObjectPatProp::KeyValue(kv) => {
                            if let ast::PropName::Computed(c) = &kv.key {
                                c.expr.visit_with(self);
                            }
                            self.visit_pat_expressions(&kv.value);
                        }
                        ast::ObjectPatProp::Assign(assign) => {
                            if let Some(val) = &assign.value {
                                val.visit_with(self);
                            }
                        }
                        ast::ObjectPatProp::Rest(rest) => self.visit_pat_expressions(&rest.arg),
                    }
                }
            }
            ast::Pat::Rest(rest) => self.visit_pat_expressions(&rest.arg),
            ast::Pat::Assign(assign) => {
                self.visit_pat_expressions(&assign.left);
                assign.right.visit_with(self);
            }
            ast::Pat::Expr(e) => e.visit_with(self),
            ast::Pat::Invalid(_) => {}
        }
    }
}

impl Visit for ReadIdentCollector {
    fn visit_ident(&mut self, node: &ast::Ident) {
        self.record_read(node.sym.as_ref());
    }

    fn visit_member_prop(&mut self, node: &ast::MemberProp) {
        if let ast::MemberProp::Computed(c) = node {
            c.expr.visit_with(self);
        }
    }

    fn visit_prop(&mut self, node: &ast::Prop) {
        match node {
            ast::Prop::Shorthand(ident) => self.record_read(ident.sym.as_ref()),
            ast::Prop::KeyValue(kv) => {
                if let ast::PropName::Computed(c) = &kv.key {
                    c.expr.visit_with(self);
                }
                kv.value.visit_with(self);
            }
            ast::Prop::Assign(assign) => {
                self.record_read(assign.key.sym.as_ref());
                assign.value.visit_with(self);
            }
            ast::Prop::Getter(g) => {
                if let ast::PropName::Computed(c) = &g.key {
                    c.expr.visit_with(self);
                }
                if let Some(body) = &g.body {
                    body.visit_with(self);
                }
            }
            ast::Prop::Setter(s) => {
                if let ast::PropName::Computed(c) = &s.key {
                    c.expr.visit_with(self);
                }
                let mut params = Vec::new();
                collect_pat_idents(&s.param, &mut params);
                self.shadowed.push(params.into_iter().collect());
                if let Some(body) = &s.body {
                    body.visit_with(self);
                }
                self.shadowed.pop();
            }
            ast::Prop::Method(m) => {
                if let ast::PropName::Computed(c) = &m.key {
                    c.expr.visit_with(self);
                }
                m.function.visit_with(self);
            }
        }
    }

    fn visit_var_declarator(&mut self, node: &ast::VarDeclarator) {
        self.visit_pat_expressions(&node.name);
        if let Some(init) = &node.init {
            init.visit_with(self);
        }
    }

    fn visit_assign_expr(&mut self, node: &ast::AssignExpr) {
        if node.op == ast::AssignOp::Assign
            && matches!(
                node.left,
                ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(_))
            )
        {
            node.right.visit_with(self);
            return;
        }
        node.left.visit_with(self);
        node.right.visit_with(self);
    }

    fn visit_function(&mut self, node: &ast::Function) {
        let mut param_names = Vec::new();
        for p in &node.params {
            self.visit_pat_expressions(&p.pat);
            collect_pat_idents(&p.pat, &mut param_names);
        }
        self.shadowed.push(param_names.into_iter().collect());
        if let Some(body) = &node.body {
            body.visit_with(self);
        }
        self.shadowed.pop();
    }

    fn visit_arrow_expr(&mut self, node: &ast::ArrowExpr) {
        let mut param_names = Vec::new();
        for p in &node.params {
            self.visit_pat_expressions(p);
            collect_pat_idents(p, &mut param_names);
        }
        self.shadowed.push(param_names.into_iter().collect());
        node.body.visit_with(self);
        self.shadowed.pop();
    }

    fn visit_fn_decl(&mut self, node: &ast::FnDecl) {
        node.function.visit_with(self);
    }

    fn visit_catch_clause(&mut self, node: &ast::CatchClause) {
        let mut bound = Vec::new();
        if let Some(pat) = &node.param {
            collect_pat_idents(pat, &mut bound);
        }
        self.shadowed.push(bound.into_iter().collect());
        node.body.visit_with(self);
        self.shadowed.pop();
    }
}

fn collect_pat_idents(pat: &ast::Pat, out: &mut Vec<String>) {
    match pat {
        ast::Pat::Ident(ident) => out.push(ident.id.sym.to_string()),
        ast::Pat::Array(arr) => {
            for elem in arr.elems.iter().flatten() {
                collect_pat_idents(elem, out);
            }
        }
        ast::Pat::Object(obj) => {
            for prop in &obj.props {
                match prop {
                    ast::ObjectPatProp::KeyValue(kv) => collect_pat_idents(&kv.value, out),
                    ast::ObjectPatProp::Assign(assign) => out.push(assign.key.sym.to_string()),
                    ast::ObjectPatProp::Rest(rest) => collect_pat_idents(&rest.arg, out),
                }
            }
        }
        ast::Pat::Rest(rest) => collect_pat_idents(&rest.arg, out),
        ast::Pat::Assign(assign) => collect_pat_idents(&assign.left, out),
        _ => {}
    }
}

fn root_member_ident(mut expr: &ast::Expr) -> Option<String> {
    loop {
        match expr {
            ast::Expr::Ident(ident) => return Some(ident.sym.to_string()),
            ast::Expr::Member(member) => expr = &member.obj,
            ast::Expr::Paren(paren) => expr = &paren.expr,
            _ => return None,
        }
    }
}

#[derive(Default)]
struct MutatedIdentCollector {
    names: BTreeSet<String>,
}

impl Visit for MutatedIdentCollector {
    fn visit_assign_expr(&mut self, node: &ast::AssignExpr) {
        match &node.left {
            ast::AssignTarget::Simple(ast::SimpleAssignTarget::Ident(ident)) => {
                self.names.insert(ident.id.sym.to_string());
            }
            ast::AssignTarget::Simple(ast::SimpleAssignTarget::Member(member)) => {
                if let Some(root) = root_member_ident(&member.obj) {
                    self.names.insert(root);
                }
            }
            ast::AssignTarget::Pat(pat) => {
                let mut bound = Vec::new();
                match pat {
                    ast::AssignTargetPat::Array(a) => {
                        for elem in a.elems.iter().flatten() {
                            collect_pat_idents(elem, &mut bound);
                        }
                    }
                    ast::AssignTargetPat::Object(o) => {
                        for prop in &o.props {
                            match prop {
                                ast::ObjectPatProp::KeyValue(kv) => collect_pat_idents(&kv.value, &mut bound),
                                ast::ObjectPatProp::Assign(assign) => bound.push(assign.key.sym.to_string()),
                                ast::ObjectPatProp::Rest(rest) => collect_pat_idents(&rest.arg, &mut bound),
                            }
                        }
                    }
                    ast::AssignTargetPat::Invalid(_) => {}
                }
                self.names.extend(bound);
            }
            _ => {}
        }
        node.visit_children_with(self);
    }

    fn visit_update_expr(&mut self, node: &ast::UpdateExpr) {
        if let Some(root) = root_member_ident(&node.arg) {
            self.names.insert(root);
        }
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &ast::CallExpr) {
        if let ast::Callee::Expr(callee) = &node.callee
            && let ast::Expr::Member(member) = &**callee
            && let ast::MemberProp::Ident(prop) = &member.prop
            && matches!(
                prop.sym.as_ref(),
                "push" | "pop" | "shift" | "unshift" | "splice" | "sort" | "reverse" | "set" | "add" | "delete" | "clear"
            )
            && let Some(root) = root_member_ident(&member.obj)
        {
            self.names.insert(root);
        }
        node.visit_children_with(self);
    }
}

fn canonical_inline_key(value: &Value) -> Option<String> {
    let key = match value {
        Value::String(s) => {
            if s.chars().count() < INLINE_DEDUP_MIN_CHARS || s.chars().count() > INLINE_LIMIT_CHARS {
                return None;
            }
            if (s.starts_with('{') || s.starts_with('['))
                && let Ok(parsed) = serde_json::from_str::<Value>(s)
            {
                serde_json::to_string(&parsed).unwrap_or_else(|_| s.clone())
            } else {
                s.clone()
            }
        }
        other => {
            let s = serde_json::to_string(other).ok()?;
            if s.chars().count() < INLINE_DEDUP_MIN_CHARS || s.chars().count() > INLINE_LIMIT_CHARS {
                return None;
            }
            s
        }
    };
    Some(key)
}

fn is_file_handle_obj(obj: &serde_json::Map<String, Value>, is_virtual_param: bool) -> bool {
    obj.get("content").and_then(Value::as_str).is_some()
        && (obj.get("path").and_then(Value::as_str).is_some()
            || (is_virtual_param && obj.get("file").and_then(Value::as_str).is_some()))
}

fn file_handle_signature(obj: &serde_json::Map<String, Value>, is_virtual_param: bool) -> String {
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| {
            is_virtual_param
                .then(|| obj.get("file").and_then(Value::as_str))
                .flatten()
        })
        .unwrap_or("");
    let content = obj.get("content").and_then(Value::as_str).unwrap_or("");
    let content_chars = content.chars().count();
    let line_count = content.lines().count().max(usize::from(!content.is_empty()));
    let ref_part = obj
        .get("__ref")
        .and_then(Value::as_str)
        .map(|r| format!(" | {r}"))
        .unwrap_or_default();
    let asset_part = obj
        .get("__asset_id")
        .and_then(Value::as_str)
        .map(|id| format!(" | asset: {id}"))
        .unwrap_or_default();
    format!(
        "[File({path:?}){ref_part} | {line_count} lines, {content_chars} chars{asset_part} | .slice() .grep() .outline()]"
    )
}

pub(super) fn format_binding(
    name: &str,
    value: &Value,
    tracker: &mut DisplayTracker<'_>,
) -> String {
    // 1. File Resource Handle `{ path, content: string, __ref?: string, ... }`
    if let Some(obj) = value.as_object()
        && is_file_handle_obj(obj, tracker.is_virtual_param)
    {
        let content = obj.get("content").and_then(Value::as_str).unwrap_or("");
        let path = obj
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| {
                tracker
                    .is_virtual_param
                    .then(|| obj.get("file").and_then(Value::as_str))
                    .flatten()
            })
            .unwrap_or("");
        let ref_id = obj.get("__ref").and_then(Value::as_str);
        let content_chars = content.chars().count();
        let line_count = content.lines().count().max(usize::from(!content.is_empty()));
        let sig = format!(
            "// => {name} = {}",
            file_handle_signature(obj, tracker.is_virtual_param)
        );
        let h = fnv1a64(content);

        // Pass-by-Handle, same-step alias, or turn-scoped unchanged re-read: stay compact!
        match tracker.claim_file_display(path, ref_id, h) {
            FileDisplayState::SameStepOrDelegated => return sig,
            FileDisplayState::UnchangedInTurn {
                prior_ref,
                prior_step,
            } => {
                let effective_ref = ref_id
                    .filter(|r| !r.is_empty())
                    .or_else(|| (!prior_ref.is_empty()).then_some(prior_ref.as_str()));
                let ref_part = effective_ref
                    .map(|r| format!(" | {r}"))
                    .unwrap_or_default();
                let asset_part = obj
                    .get("__asset_id")
                    .and_then(Value::as_str)
                    .map(|id| format!(" | asset: {id}"))
                    .unwrap_or_default();
                return format!(
                    "// => {name} = [File({path:?}){ref_part} | {line_count} lines, {content_chars} chars{asset_part} | unchanged since step {prior_step} — reuse live handle via {name}.slice()/{name}.grep()]"
                );
            }
            FileDisplayState::Show => {}
        }
        if content_chars <= INLINE_LIMIT_CHARS && tracker.take_inline_budget(content_chars) {
            tracker.record_file_shown(path, ref_id, h);
            let ref_part = ref_id.map(|r| format!("{r}, ")).unwrap_or_default();
            let label = format!("{name} (File {path:?}, {ref_part}{line_count} lines)");
            return format_comment_block(&label, &add_line_numbers(content, 1));
        }
        tracker.record_file_shown(path, ref_id, h);
        return format!(
            "{sig}\n{}",
            format_large_source_index(&format!("{name}.content ({path})"), content, Some(path))
        );
    }

    // 2. Command Resource Handle `{ exit_code, stdout, stderr, __ref?: string, ... }`
    if let Some(obj) = value.as_object()
        && obj.contains_key("exit_code")
        && (obj.contains_key("stdout") || obj.contains_key("stderr"))
    {
        let ref_id = obj.get("__ref").and_then(Value::as_str);
        let exit = obj["exit_code"].clone();
        let stdout = obj.get("stdout").and_then(Value::as_str).unwrap_or("");
        let stderr = obj.get("stderr").and_then(Value::as_str).unwrap_or("");
        let combined = if stderr.is_empty() {
            stdout.to_owned()
        } else if stdout.is_empty() {
            stderr.to_owned()
        } else {
            format!("{stdout}\n--- stderr ---\n{stderr}")
        };
        let chars = combined.chars().count();
        let lines = combined.lines().count();
        let ref_part = ref_id.map(|r| format!(" | {r}")).unwrap_or_default();
        let asset_part = obj
            .get("__asset_id")
            .and_then(Value::as_str)
            .map(|id| format!(" | asset: {id}"))
            .unwrap_or_default();
        let sig = format!(
            "// => {name} = [CommandResult(exit_code={exit}){ref_part} | {lines} lines, {chars} chars{asset_part} | .slice() .grep() .tail()]"
        );
        let h = fnv1a64(&combined);
        if !tracker.claim_display(ref_id, Some(h)) {
            return sig;
        }
        if chars <= INLINE_LIMIT_CHARS && tracker.take_inline_budget(chars) {
            let label = format!("{name} (exit_code={exit})");
            return format_comment_block(&label, &combined);
        }
        return format!("{sig}\n{}", format_large_log_index(name, &exit, &combined));
    }

    // 3. Plain string
    if let Some(text) = value.as_str() {
        let chars = text.chars().count();
        let lines = text.lines().count().max(usize::from(!text.is_empty()));
        if text.is_empty() {
            return format!("// => {name} = \"\" (Empty string)");
        }
        let sig = format!(
            "// => {name} = [string | {lines} lines, {chars} chars | > 64KB not inlined]"
        );
        let h = fnv1a64(text);
        if chars > 2_000 && !tracker.claim_display(None, Some(h)) {
            return format!("// => {name} = [string | {lines} lines, {chars} chars | already shown]");
        }
        if chars <= INLINE_LIMIT_CHARS && tracker.take_inline_budget(chars) {
            if text.contains('\n') {
                return format_comment_block(name, text);
            }
            return format!("// => {name} = {text:?}");
        }
        return format!("{sig}\n{}", format_large_source_index(name, text, None));
    }

    // 4. Array (with compact summary for Array of File Resource Handles)
    if let Some(arr) = value.as_array() {
        if arr.is_empty() {
            return format!("// => {name} = [] (Empty: 0 items)");
        }
        let all_file_handles = arr.iter().all(|item| {
            item.as_object()
                .is_some_and(|o| is_file_handle_obj(o, tracker.is_virtual_param))
        });
        if all_file_handles {
            let mut summaries: Vec<String> = Vec::with_capacity(arr.len());
            let mut expanded_blocks: Vec<String> = Vec::new();
            for (idx, item) in arr.iter().enumerate() {
                let p = item["path"].as_str().unwrap_or("");
                let c = item["content"].as_str().unwrap_or("");
                let r = item.get("__ref").and_then(Value::as_str);
                let h = fnv1a64(c);
                let base_chip = match r {
                    Some(ref_id) => format!("{ref_id}({p:?})"),
                    None => format!("File({p:?})"),
                };
                match tracker.claim_file_display(p, r, h) {
                    FileDisplayState::SameStepOrDelegated => {
                        summaries.push(base_chip);
                    }
                    FileDisplayState::UnchangedInTurn { prior_step, .. } => {
                        summaries.push(format!("{base_chip} [unchanged since step {prior_step}]"));
                    }
                    FileDisplayState::Show => {
                        summaries.push(base_chip);
                        let chars = c.chars().count();
                        if chars <= INLINE_LIMIT_CHARS && tracker.take_inline_budget(chars) {
                            tracker.record_file_shown(p, r, h);
                            expanded_blocks.push(format_comment_block(
                                &format!("{name}[{idx}] (File {p:?})"),
                                &add_line_numbers(c, 1),
                            ));
                        } else if chars > INLINE_LIMIT_CHARS {
                            tracker.record_file_shown(p, r, h);
                            expanded_blocks.push(format_large_source_index(
                                &format!("{name}[{idx}].content ({p})"),
                                c,
                                Some(p),
                            ));
                        }
                    }
                }
            }
            let mut out = format!(
                "// => {name} = [Array({}) of Handles: {}]",
                arr.len(),
                summaries.join(", ")
            );
            for block in expanded_blocks {
                out.push('\n');
                out.push_str(&block);
            }
            return out;
        }

        // 4b. Array of strings containing multiline/long text (e.g. `[a.slice(...), b.slice(...)]`):
        // render each item cleanly unescaped and fill progressively up to the 64KB step budget
        // instead of JSON-escaping `\n`/`\"` or collapsing the entire array to 978 bytes!
        let all_strings = arr.iter().all(Value::is_string);
        let has_multiline_or_long_str = all_strings
            && arr.iter().any(|item| {
                item.as_str()
                    .is_some_and(|s| s.contains('\n') || s.chars().count() > 200)
            });
        if has_multiline_or_long_str {
            let mut blocks = Vec::with_capacity(arr.len());
            for (idx, item) in arr.iter().enumerate() {
                let text = item.as_str().unwrap_or("");
                let label = format!("{name}[{idx}]");
                blocks.push(format_progressive_string(&label, text, tracker));
            }
            return blocks.join("\n");
        }

        let serialized = serde_json::to_string(value).unwrap_or_default();
        let chars = serialized.chars().count();
        if tracker.container_already_covered(value) {
            let shape = describe_item_shape(&arr[0]);
            return format!(
                "// => {name} = [Array({}) | itemShape: {shape} | {chars} chars]",
                arr.len()
            );
        }
        if chars <= INLINE_LIMIT_CHARS && tracker.take_inline_budget(chars) {
            return format!("// => {name} = {serialized}");
        }
        let shape = describe_item_shape(&arr[0]);
        let sig = format!(
            "// => {name} = [Array({}) | itemShape: {shape} | {chars} chars]",
            arr.len()
        );
        return format!("{sig}\n{}", format_large_array_index(name, arr, &shape));
    }

    // 5. Dictionary of File Resource Handles `{ key: ResourceHandle, ... }`
    if let Some(map) = value.as_object()
        && !map.is_empty()
        && map.values().all(|v| {
            v.as_object()
                .is_some_and(|o| is_file_handle_obj(o, tracker.is_virtual_param))
        })
    {
        let mut lines = vec![format!("// => {name} = {{ // Map({}) of ResourceHandles", map.len())];
        let mut to_expand: Vec<(&str, &str, Option<&str>, &str, u64)> = Vec::new();
        for (k, v) in map {
            let obj = v.as_object().unwrap();
            let c = obj.get("content").and_then(Value::as_str).unwrap_or("");
            let p = obj.get("path").and_then(Value::as_str).unwrap_or(k);
            let r = obj.get("__ref").and_then(Value::as_str);
            let h = fnv1a64(c);
            let sig = file_handle_signature(obj, tracker.is_virtual_param);
            match tracker.claim_file_display(p, r, h) {
                FileDisplayState::SameStepOrDelegated => {
                    lines.push(format!("//   {k:?}: {sig}"));
                }
                FileDisplayState::UnchangedInTurn { prior_step, .. } => {
                    lines.push(format!(
                        "//   {k:?}: {sig} [unchanged since step {prior_step}]"
                    ));
                }
                FileDisplayState::Show => {
                    lines.push(format!("//   {k:?}: {sig}"));
                    to_expand.push((k, p, r, c, h));
                }
            }
        }
        lines.push("// }".into());
        // If newly shown content fits easily within remaining 64KB budget, expand inline!
        let total_new_chars: usize = to_expand
            .iter()
            .map(|(_, _, _, c, _)| c.chars().count())
            .sum();
        if total_new_chars <= tracker.remaining_inline_budget {
            for (k, p, r, c, h) in to_expand {
                if tracker.take_inline_budget(c.chars().count()) {
                    tracker.record_file_shown(p, r, h);
                    lines.push(format_comment_block(
                        &format!("{name}[{k:?}] (File {p:?})"),
                        &add_line_numbers(c, 1),
                    ));
                }
            }
        }
        return lines.join("\n");
    }

    // 6. Object with multiline/long strings or nested File/Command handles
    // (e.g. `({ lib: libF.slice(...), decl: decF.slice(...) })`):
    // render key-by-key cleanly unescaped and fill progressively up to the 64KB step budget.
    if let Some(map) = value.as_object()
        && !map.is_empty()
        && map.values().any(|v| {
            v.as_str()
                .is_some_and(|s| s.contains('\n') || s.chars().count() > 200)
                || v.as_object().is_some_and(|o| {
                    is_file_handle_obj(o, tracker.is_virtual_param)
                        || (o.contains_key("exit_code")
                            && (o.contains_key("stdout") || o.contains_key("stderr")))
                })
        })
    {
        let mut blocks = Vec::with_capacity(map.len());
        for (k, v) in map {
            let field_name = format!("{name}.{k}");
            if let Some(text) = v.as_str() {
                blocks.push(format_progressive_string(&field_name, text, tracker));
            } else {
                blocks.push(format_binding(&field_name, v, tracker));
            }
        }
        return blocks.join("\n");
    }

    // 7. Generic Object / Primitive
    let serialized = serde_json::to_string(value).unwrap_or_default();
    let chars = serialized.chars().count();
    let keys_summary = value
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{k}({}c)", v.to_string().chars().count()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "value".into());
    let asset_part = value
        .as_object()
        .and_then(|o| o.get("__asset_id"))
        .and_then(Value::as_str)
        .map(|id| format!(" | asset: {id}"))
        .unwrap_or_default();
    let sig = format!(
        "// => {name} = [Object {{ {keys_summary} }} | {chars} chars{asset_part}]"
    );
    if tracker.container_already_covered(value) {
        return sig;
    }
    if chars <= INLINE_LIMIT_CHARS && tracker.take_inline_budget(chars) {
        return format!("// => {name} = {serialized}");
    }
    let head: String = serialized.chars().take(1_500).collect();
    format!(
        "{sig}\n\
         // [{name} LARGE Object Index ({chars} chars) — keys: {keys_summary}]:\n\
         // Slice/filter `{name}` directly in JS (or pass `{name}` to a `@virtual` helper).\n\
         // head preview: {head}..."
    )
}

/// Format a string entry inside an array/object cleanly without JSON escaping,
/// filling progressively up to the remaining 64KB per-step budget instead of
/// collapsing the entire parent container all-or-nothing.
fn format_progressive_string(
    label: &str,
    text: &str,
    tracker: &mut DisplayTracker<'_>,
) -> String {
    if text.is_empty() {
        return format!("// => {label} = \"\" (Empty string)");
    }
    let chars = text.chars().count();
    let lines = text.lines().count().max(1);
    let h = fnv1a64(text);
    if chars > 2_000 && !tracker.claim_display(None, Some(h)) {
        return format!("// => {label} = [string | {lines} lines, {chars} chars | already shown]");
    }
    if chars <= INLINE_LIMIT_CHARS && tracker.take_inline_budget(chars) {
        if text.contains('\n') {
            return format_comment_block(label, text);
        }
        return format!("// => {label} = {text:?}");
    }
    let avail = tracker.remaining_inline_budget;
    if avail >= 1_024 {
        let mut head: String = text.chars().take(avail).collect();
        // Prefer cutting at a newline boundary so the last printed line stays complete.
        if let Some(last_nl) = head.rfind('\n')
            && last_nl >= avail / 2
        {
            head.truncate(last_nl);
        }
        let shown_chars = head.chars().count();
        let _ = tracker.take_inline_budget(shown_chars);
        let omitted = chars.saturating_sub(shown_chars);
        let body = format!(
            "{head}\n... [truncated: {omitted} more chars omitted — 64KB step budget reached; inspect remaining via `{label}` or source handle]"
        );
        return format_comment_block(label, &body);
    }
    format!(
        "// => {label} = [string | {lines} lines, {chars} chars | omitted: 64KB step budget reached — inspect via `{label}` or source handle]"
    )
}
