---
name: code-index
description: Query existing Rust (syn) and TS/JS (SWC) source indexes for symbols, signatures, source ranges and syntax-based callers/callees instead of writing parsers or scanning every file.
---

# Source index library

Use `require("code_index")` inside a module implementation. Browse methods with `read_tool_contracts({libraries:["code_index"],overview:true})`, then request exact methods as needed. Reading this skill is optional for linking the library.

All methods accept `language: "rust" | "typescript"` and optional `workspace_root` (defaults to the current workspace; relative roots resolve within it). `typescript` includes JS, JSX and TSX. Discover the actual stack first.

- `status`: cache existence and coverage/freshness information.
- `index`: build or refresh the existing language index. Only the index cache changes; source files are not edited.
- `list`: paged symbol outlines, optionally scoped to a file or directory.
- `search`: ranked, paged symbol search; use file/directory filters to narrow evidence.
- `read`: exact current symbol source, positions and syntax-based callers/callees. Select by `symbol_id` from search, or by `file_path` and `name`.

Queries maintain the index automatically, including initializing a missing cache. Start from an entry point or known reference: locate its symbol within the relevant file/directory, use `read` for the implementation, then follow the observed caller/callee references. Follow `page.next_offset` when needed. Inspect `index.coverage_issues` and report omissions; an empty result does not prove absence when parsing failed. Relationships are syntax-based, not full type resolution, macro expansion or runtime coverage.

Pass selected source and positions to a focused `@virtual` for complex understanding and judgment. Ordinary code calls the library, organizes inputs and checks results. Use ordinary file tools for unsupported files or missing evidence.

These methods run in the normal implementation, not `probe:true` (probes only permit the core file-reading tools).
