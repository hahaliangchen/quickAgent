# Scope & Trap Audit (`baseline-16m9s` -> HEAD)

Read-only audit of all changes vs `baseline-16m9s`.

---

## 1. Opened Scopes & Isolation Boundaries

### 1.1 `process.run_readonly` / `run_readonly_command`
- **Opened**:
  - `src/agent_program/libraries.rs:127`: added `("run_readonly", "run_readonly_command")` to builtin `process`.
  - `src/single_agent/program_flow.rs:32-57,77-84`: `main_execution_allowed(s)` (`!s.ban_execution && code_shell_context(s)`), `shell_code_allowed(s, w)` returns `true` when `w.program_task.is_none()` (top-level `write_program`), and `compile_static_libraries` dropped `body &&`.
  - `src/single_agent.rs:357,429,1477-1484`: injects `read_only_definition()`.
- **Isolation checks**:
  - **`ban_execution` & `chat` session & `plugin`**: **Preserved.** `main_execution_allowed` (`program_flow.rs:32-34`), `read_only_shell` (`single_agent.rs:1368-1370`), and `code_shell_context` (`single_agent.rs:1372-1379`) all block `s.ban_execution`, `s.plugin.is_some()`, and `agent_session_meta.kind == "chat"`.
  - **`probe: true`**: **Preserved.** `probe_read` (`program_flow.rs:735-741`) excludes `run_readonly_command` and `single_agent.rs:4546-4549` enforces `probe_read`.
  - **`@virtual` leaf boundary leak (`js_tools` / `process` library)**: **BROKEN / OVER-OPENED.**
    - `src/project_process.rs:288-294` `is_execution()` does **not** include `"run_readonly_command"`.
    - `catalog_with_execution` (`program_flow.rs:58-69`) only strips `project_process::is_execution`. Thus `library_catalog` retains `process.run_readonly -> run_readonly_command` even when `catalog_execution_allowed(s)` is `false` on a `@virtual` leaf.
    - If a program passes `host.context({ libraries: ["process"] })` or `tools: ["run_readonly_command"]` to a `@virtual` leaf (`program_flow.rs:439-458`), `task["js_tools"]` receives `"run_readonly_command"`. Then `tools()` (`single_agent.rs:357`) injects `read_only_definition()` even when `is_leaf == true`, and `execute_call` (`single_agent.rs:4623-4628`) explicitly permits it via `js_allows_readonly` (`direct_ok || program_ok`), contradicting `prompts` / `request()` (`single_agent.rs:1918`: shell methods *"never in `@virtual` leaves"*).
  - **Duplicated injection & stale error text**:
    - `read_only_definition()` is conditionally pushed in **3 separate places** (`single_agent.rs:357,429,1477`), requiring `!definitions.iter().any(...)` dedup guards in all 3.
    - `business_tool_inner` (`single_agent.rs:2583-2584`) still says `"Shell runs in the code implementation of a first-layer virtual function..."`, which is stale now that top-level `write_program` also runs `exec_command`.

### 1.2 Inflated Thresholds (`弄巧成拙` / Mismatch)
- `PAGE_CHARS` (`single_agent.rs:37`) / `MAX_PAGE_CHARS` (`context_assets.rs:14`): `24k -> 48k` (clamped at `context_assets.rs:672,752` instead of erroring — good).
- `DeliverySoftLimits` (`single_agent.rs:50-59`): `1/4 -> 1/2` of `input_budget` (floor `64k` tokens / `192k` chars). On a `1M`-token model, `round_chars` is `~1.5M` chars, yet `STEP_WRAP_ROUND_CHARS` (`single_agent.rs:152`) is `240_000` chars (`~70k` tokens). So `delivery_soft_limits` lets one round ingest 250k+ chars without `MATERIAL_OVER_CAPACITY`, and `maybe_require_step_wrap` immediately forces a wrap on round 1!
- Other doubled limits (`MAIN_CONTEXT_PATCH_LIMIT_BYTES` `64 KiB`, `MAIN_CONTEXT_TOTAL_LIMIT_BYTES` `128 KiB`, `READ_ONLY_PREVIEW_CHARS` `96 KiB`, `CHAT_CHARS` `80k`, `frame_index` `128 / 6000 tokens`) have no boundary leak, though they allow larger context growth before wraps.

---

## 2. `maybe_require_step_wrap` (`src/single_agent.rs:1299-1363`)
- **Bug (`:1326-1327`)**:
  ```rust
  let bloated = round_added_chars >= STEP_WRAP_ROUND_CHARS
      || (w.step_rounds_in_scope >= 2 && current_tokens >= token_threshold);
  ```
  `round_added_chars >= STEP_WRAP_ROUND_CHARS` (`240_000`) has **no `step_rounds_in_scope` check and no `current_tokens` check**. Because `w.step_rounds_in_scope` increments at `:1315`, a single parallel read batch (`~250k` chars, `~70-85k` total tokens) triggers `VIRTUAL_STEP_WRAP_DIRECTIVE` on **round 1** (`step_rounds_in_scope == 1`), blocking further reads (`:4552-4560`) even when `current_tokens` is well under `STEP_WRAP_WINDOW_TOKENS` (`240k`).
- **Locked-in test**: `host_forces_virtual_step_wrap_when_round_bloats_and_blocks_more_reads` (`src/single_agent.rs:7480-7580`) fires 5 × 50k-char `read_file` calls in step 0 (`round 1`) and asserts `VIRTUAL_STEP_WRAP_DIRECTIVE` in step 1. Both `:1326-1327` and this test must change together if gating by `w.step_rounds_in_scope >= 3 || current_tokens >= token_threshold`.

---

## 3. `event_seq` Delivery (`src/single_agent.rs:3967-4090` & `request()` `:2083-2136`)
- **Non-object results drop `event_seq`**:
  - `run_rounds` (`:3984`): `if !plain_leaf_result && let Some(object) = delivery.as_object_mut()` silently skips non-object results such as `list_skills` (`:4676`, returns JSON array).
  - `request()` (`:2126`): `if let Some(seq) = packet.get("event_seq").cloned() && let Some(obj) = unwrapped.as_object_mut()` silently drops `event_seq` if `original_result` is a JSON array (e.g. `parallel_read_batch` `:4612`, which returns `json!(results)` and IS a material tool).
- **Roundabout wrap-then-unwrap (`弄巧成拙`)**:
  - For material tools (`read_file`, `search_text`, `list_dir`, `run_readonly_command`, `refresh_asset`), `run_rounds` (`:3970-3993`) wraps `result` in `{"asset_reference", "original_result", "event_seq", "delivery_reference"}` via `context_assets::delivered`, stores that in `w.messages`, and then `request()` (`:2117-2133`) re-parses `message["content"]` on every Main model request to unwrap `original_result` and copy `event_seq` onto `unwrapped`.
  - **Consequences**:
    1. Tools NOT in `library_catalog` (e.g. `read_skill` at `:4684,4689`, plugin/non-library tools) are never in `library_calls` (`:2083-2106`), so `request()` leaves them in the raw `{asset_reference, original_result, event_seq, delivery_reference}` wrapper instead of unwrapping.
    2. In `@virtual` leaves (`w.program_task.is_some()`), `plain_leaf_result` (`:3977-3983`) skips adding `event_seq` (`:3984`), **but** `:3972` still wraps material results in `{asset_reference, original_result}` and `:3992` still adds `delivery_reference`, AND `request()` only unwraps when `role == "main"` (`:2072`). So a `@virtual` leaf sees the wrapped envelope (without `event_seq`), while `read_file` in `main` sees unwrapped `original_result` + `event_seq`.
    3. `leaf_over_capacity` (`:3323-3331`) omits `event_seq` (unlike `material_over_capacity` `:3310`).
  - **Simplification**: Put `event_seq` (when desired) directly on the stored payload or unwrap consistently in one helper by checking `packet["asset_reference"].is_object()` directly rather than gating on `library_calls` (`request()` `:2108-2134`) + `!plain_leaf_result`. (Note: why `if !library_result` existed originally — to avoid mutating JS/leaf library return schemas, which only broke because `Main` calls the same `familiar` tools that also appear in `library_catalog`, and `asset_reference` wrapping hid `event_seq` on the outer envelope.)

---

## 4. `build_slice_entry`, `finalize_text_slice` (`single_agent.rs:677-849`) & `read_event_payload` (`context_assets.rs:540-633`)
- **Bug 1 (`list_dir` loses `"path"` on `grep`)**: `ListDirResponse` (`src/tools.rs:878-882`) has `{root, entries: [{path, kind, size_bytes, modified_at, modified_ms}], truncated}`. `build_slice_entry` has no `entries` branch, so `list_dir` hits the fallback `serde_json::to_string_pretty(&payload)` (`:841`). Each entry spans ~7 lines; `grep: "file"` / `grep: "modified_at"` matches only the `"kind"` or `"modified_at"` line and **loses `"path"`**, and `head`/`tail` cut mid-JSON-object.
- **Bug 2 (Empty-stdout `exit_code: 0` commands falsely fail as `empty_match`)**: `:797-813` collects `stdout.lines()` + `stderr.lines()`. When both are `""`, `pairs` is empty `[]`, `slice_numbered_lines` returns `range: None`, and `finalize_text_slice` (`:684-689`) returns `status: "empty_match"` even when the selector was just `{seq}` with **no** `lines`/`grep`/`head`/`tail` filter — causing `attach_handoff_context` (`:1187`) to drop it into `skipped_slices`.
- **Bug 3 (`search_text` with `matches: []` OR `files` mode falsely fails as `empty_match`)**:
  - `SearchTextResponse` (`src/tools.rs:478-484`) ALWAYS serializes `"matches": []` even when 0 matches or when `output_mode` is `files` / `counts`.
  - In `build_slice_entry` (`:818-833`), `if let Some(matches) = payload["matches"].as_array()` ALWAYS matches `matches: []`, making `else if let Some(files) = payload["files"].as_array()` at `:826` **dead code** for `search_text`! Both 0-hit `{seq}` proofs and `files`-mode results produce `pairs = []` -> falsely rejected as `empty_match`.
- **Bug 4 (`read_event_payload` `context_assets.rs:540-633`)**:
  - Snapshot preference works for registered assets (`:541-552`) and unwraps `read_asset` (`:600-631`). However, for `is_error == true` / `OutputCapacityError` commands (not registered as assets, `:4869-4895`), it falls back to `agent_task_events` (`/meta/result`), which holds truncated preview JSON (`shell_process.rs:929-944`).
- **Uniform simplification for `build_slice_entry`**:
  1. Distinguish **empty payload with no filter** vs **filter matched nothing**: track `has_filter = lines.is_some() || grep.is_some()`. If `pairs.is_empty() && !has_filter`, return a valid summary slice (`excerpt: ""`, keeping `exit_code` / `match_count: 0` / `entry_count: 0`) instead of `empty_match`. Only emit `empty_match` when a filter/range/keys selector matched 0 items.
  2. Replace the 4 ad-hoc format branches (`:766-848`) with one small `extract_payload_lines(&payload) -> (Map, Vec<(usize, String)>)` mapper producing 1 logical line per item:
     - `path` + (`lines` or `content`) -> real `(line_no, text)`
     - `exit_code` -> `stdout` + `[stderr]` lines
     - `entries` (`list_dir`) -> `"{path} ({kind}, {size_bytes}B)"`
     - `matches` (if non-empty), else `files` (if non-empty), else `counts` (`search_text`) -> 1 line per hit/file/count
     - `commits` (`git_log`) / `files` (`git_diff` / `git_status`) -> 1 line per entry
     - fallback -> `pretty` lines.
  3. Feed into single `slice_numbered_lines` + `finalize_text_slice` path (handling `keys` orthogonally).

---

## 5. `end_familiarization` (`program_flow.rs:796-843`) & `reset_messages_for_virtual_step` (`single_agent.rs:1256-1297`)
- **No duplicate `virtual_step_handoff`**: Both functions prevent duplicate `VIRTUAL_STEP_HANDOFF` across interleaved `handoff_step` + `write_program` calls (`reset_messages_for_virtual_step` strips prior `VIRTUAL_STEP_HANDOFF` across both `< prefix_end` `:1265` and `>= prefix_end` `:1270-1289` before pushing the new one at `:1291`; `end_familiarization` preserves existing `VIRTUAL_STEP_HANDOFF` at `:809`).
- **Drift & Accumulation traps**:
  1. **`program_diagnostic` dropped on `handoff_step`**: `end_familiarization` (`:824-830`) converts `program_diagnostic` into a 1-line `program_record`, whereas `reset_messages_for_virtual_step` (`:1270-1289`) silently drops `program_diagnostic`.
  2. **Unbounded `program_result` / `program_record` accumulation**: `reset_messages_for_virtual_step` (`:1273`) keeps every `program_result` and `program_record` verbatim. Multiple `write_program` -> `handoff_step` cycles in one turn accumulate all prior `program_result` bodies (each up to `1/2 input_budget`) before the new `virtual_step_handoff`, defeating scope cleanup.
  3. **Duplicated retention predicate**: `end_familiarization` (`:804-841`) and `reset_messages_for_virtual_step` (`:1263-1290`) duplicate almost identical keep-lists (`is_user_original`, `program_result`, `program_record`, `DELIVERED`, `modules::UPDATE`, `ADDED_TOOLS`, `_pinned`), with slight boundary divergence (`rposition(is_user_original && name.is_none())` vs `position(is_user_original)`). Should share one retention helper.
