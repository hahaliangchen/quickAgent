# Read-Only Code Mapping Report (`threshold-mapper`)

Scope inspected: `src/single_agent.rs`, `src/single_agent/program_flow.rs`, `src/single_agent/task_frames.rs`, `src/context_assets.rs`, `prompts/single_agent.md` (plus constant references in `src/source_read.rs`, `src/shell_process.rs`, `src/tools.rs`).

---

## 1. Context Growth, Wrap, Handoff, Slice, Ledger & Asset Constants

### A. Virtual Step Wrapping & `handoff_step` Budgets (`src/single_agent.rs`)

| Constant / Threshold | Location | Current Value | Usage |
|---|---|---|---|
| `MAIN_CONTEXT_PATCH_LIMIT_BYTES` | `src/single_agent.rs:149` | `32 * 1024` (32 KiB) | Max serialized `main_context_patch` in `prepare_handoff_step` (`:1073`) |
| `MAIN_CONTEXT_TOTAL_LIMIT_BYTES` | `src/single_agent.rs:150` | `64 * 1024` (64 KiB) | Max merged `main_context` in `prepare_handoff_step` (`:1089`) |
| `MAX_STEP_HANDOFFS_PER_TURN` | `src/single_agent.rs:151` | `12` | Max `handoff_step` calls per turn (`:1044`, `:1260`) |
| `STEP_WRAP_ROUND_CHARS` | `src/single_agent.rs:152` | `120_000` | Round delivered-char trigger in `maybe_require_step_wrap` (`:1269`) |
| `STEP_WRAP_WINDOW_TOKENS` | `src/single_agent.rs:153` | `120_000` | Token ceiling in `maybe_require_step_wrap` (`:1268`: `.min((s.input_budget / 2).max(1))`, requires `w.step_rounds_in_scope >= 2` at `:1270`) |
| `DEFAULT_HANDOFF_SLICE_ITEM_CHARS` | `src/single_agent.rs:154` | `4_000` | Default `max_chars` per slice (`slice_numbered_lines` `:670`, `build_slice_entry` `:700`) |
| `MAX_HANDOFF_SLICE_ITEM_CHARS` | `src/single_agent.rs:155` | `12_000` | Clamp & schema max per slice (`:557`, `:671`, `:701`) |
| `MAX_HANDOFF_SLICES_TOTAL_BYTES` | `src/single_agent.rs:156` | `24 * 1024` (24 KiB) | Total `context_slices` budget in `merge_handoff_slices` (`:861`, `:866`) |
| `MAX_HANDOFF_SELECTORS_PER_CALL` | `src/single_agent.rs:157` | `16` | Max `slices` selectors per `handoff_step` (`:535` schema `maxItems`, `:1139` `.take(...)`) |

### B. `workspace_ledger` Caps (`src/single_agent.rs:896-1040`)

| Cap | Location | Current Value | Behavior |
|---|---|---|---|
| Command display truncation | `src/single_agent.rs:984` | `80` chars | `truncate_chars(cmd_raw, 80)` |
| Max files in ledger | `src/single_agent.rs:1001` | `40` | `file_list.truncate(40)` (sorted by `latest_seq` desc) |
| Max `read_ranges` per file | `src/single_agent.rs:1016` | `6` | `.take(6)` merged `(start, end)` ranges |
| Max commands in ledger | `src/single_agent.rs:1025-1027` | `12` | Retains newest `12` commands (`split_off(commands.len() - 12)`) |

### C. Delivery Soft Limits & Inline / Asset / `frame_index` / Turn-Carry Limits

| Constant / Threshold | Location | Current Value | Behavior / Notes |
|---|---|---|---|
| `PAGE_CHARS` | `src/single_agent.rs:37` | `24_000` | `read_asset` schema `max_chars` (`:1353`), `material_over_capacity` `next_read` (`:3244`), `task_frames::fit_input_bodies` (`task_frames.rs:924`); string literal `24000` also in `:3235` |
| `delivery_soft_limits` | `src/single_agent.rs:50-59` | `(input_budget / 4).max(32_000)` tokens; `* 3` chars | Used in `run_rounds` (`:3946-3952`) & `program_flow::delivery_capacity` (`program_flow.rs:1640-1644`). Note: `fits_window` reserves `4096` tokens (`:3944`, `:3986`, `program_flow.rs:1641`, `task_frames.rs:934`). |
| `compact_cut_off_context` | `src/single_agent.rs:2468, 2482` | `EXCERPT_CHARS = 512`; call args `1024` | Truncates cut-off assistant `content` to 512 chars and replaces tool call `arguments` `> 1024` bytes with `"{}"` |
| `frame_index` SQL & token caps | `src/context_assets.rs:889-890, 928, 950` | `LIMIT 65` / `.take(64)` / `3000` tokens | Caps `task_asset_index` query at 64 entries and estimated `3000` tokens |
| `context_assets` hard-coded page cap | `src/context_assets.rs:289, 420, 633, 636, 716` | `24000` | `read()` / `register()` / `read_reference` page limit |
| `index_arguments` min size | `src/context_assets.rs:1009` | `2048` bytes | Registers assistant tool arguments >= 2048 bytes as assets |
| `CHAT_CHARS` | `src/single_agent/task_frames.rs:648` | `40_000` | Earlier chat history carried across turns (`:731`) |
| `LATEST_FILES_CHARS` | `src/single_agent/task_frames.rs:650` | `24_000` | Latest turn's `.calls.js`/programs budget across turns (`:803`) |
| `FILE_CHARS` | `src/single_agent/task_frames.rs:652` | `12_000` | Single-file cap in `chat_context` (`:810`) |
| `MAX_SCOPE_ENTRY_BYTES` / `MAX_SCOPE_TOTAL_BYTES` | `src/single_agent/program_flow.rs:1073-1074` | `4 * 1024` / `16 * 1024` | Child task scope snapshot caps |
| `source_read::{PAGE_CHARS, DEFAULT_PAGE_CHARS}` | `src/source_read.rs:15-16` | `60_000` / `24_000` | Direct `read_file` / `read_file_lines` paging (`PROGRAM_PAGE_CHARS = 2_000_000` at `:20`) |
| `shell_process::READ_ONLY_PREVIEW_CHARS` | `src/shell_process.rs:21` | `48 * 1024` | `run_readonly_command` inline preview cap (`max_output_tokens` default `4000`, max `24_000` at `:694, :783`) |
| `tools::{DEFAULT_MAX_READ_BYTES, DEFAULT_CHUNK_BYTES}` | `src/tools.rs:35-36` | `1024 * 1024` / `64 * 1024` | File read byte limit & chunk size |

### D. Unit Tests Asserting Old Constants (Must Update If Doubled)

- `src/single_agent.rs:7037-7050` (`delivery_soft_limits_scale_to_quarter_of_model_budget`): asserts `1/4` (`256_356`, `769_068`, `INPUT_BUDGET / 4`, `>= 170_000`, `32_000`) — only if `delivery_soft_limits` is changed.
- `src/single_agent.rs:7053-7204` (`handoff_step_clears_step_noise_enforces_budget_and_keeps_constraints`): `:7098` sends `"z".repeat(34_000)` expecting `MAIN_CONTEXT_OVER_LIMIT` against `MAIN_CONTEXT_PATCH_LIMIT_BYTES` (`32 KiB`). Doubling to `64 KiB` breaks this assertion unless payload > 64 KiB.
- `src/single_agent.rs:7207-7322` (`handoff_step_preserves_pinned_assets_and_carries_across_turns`): `:7243` (`30_000`) + `:7258` (`25_000`) + `:7272` (`15_000` -> total `70_000`) expects `over_total.is_err()` against `64 KiB`. Doubling total limit to `128 KiB` breaks `assert!(over_total.is_err())` at `:7277`.
- `src/single_agent.rs:7404-7542` (`host_forces_virtual_step_wrap_when_round_bloats_and_blocks_more_reads`): `:7410` writes 3 files of `50_000` chars (`~150k` total) with `max_chars: 55_000` (`:7436`) to exceed `STEP_WRAP_ROUND_CHARS` (`120_000`). Doubling `STEP_WRAP_ROUND_CHARS` to `240_000` will keep this below threshold unless file sizes / read sizes increase.
- `src/single_agent.rs:7738` & `src/context_assets.rs:59-77`: assert `24000` / `24001` page clamping/bounds if `PAGE_CHARS` changes.
- `src/source_read.rs:380, 389-390`: asserts `60_000` if `source_read::PAGE_CHARS` changes.
- `src/single_agent/task_frames.rs:1229`: uses `CHAT_CHARS / 2`.

---

## 2. All Mid-Turn Message Reset / Truncation / Retry / Frame-Switch Paths

1. **`compact_cut_off_context` + `cut_off_reply` (`src/single_agent.rs:2400, 2444-2446, 2467-2495, 3699-3702, 4117-4185`) — DIRECT MATCH FOR "输出大半了…失忆了，又从头开始了"**:
   - In `model_round` (`:2400`): `let cut_off = stats["finish_reason"] == "length";`.
   - When `cut_off || malformed` (`:2444-2446`), `compact_cut_off_context(&mut stored)` runs BEFORE `w.messages.push(stored)` (`:2460`):
     - Removes `reasoning_content`, `REASONING_ITEMS`, `THINKING_BLOCKS` (`:2470-2472`).
     - Truncates `content` to **`EXCERPT_CHARS = 512` chars** (`:2468, :2473-2479`), replacing the rest with `"\n[Truncated reply omitted from retry context; the original is saved in the record.]"`.
     - Replaces any tool-call `arguments` `> 1024` bytes or invalid JSON with `"{}"` (`:2480-2485`).
   - Then `run_rounds` (`:3699-3702`) sees `cut_off == true`, calls `cut_off_reply(s, w, &calls, &node, step, cut_off)`, and `continue`s.
   - When `calls.is_empty()` (e.g., long final answer/summary hit `finish_reason == "length"`), `cut_off_reply` (`:4176-4183`) appends a `user` message `"name": "output_limit"` (`REPLY_TRUNCATED`: `"Continue. Hand over long work as a program with write_program."`). Because `stored` in `w.messages` kept only the **first 512 chars** of the streamed answer, the model loses everything it already wrote and restarts/rewrites from scratch!
   - Also check `model_round` transport retry (`:2291-2395`): retries up to `MODEL_ATTEMPTS = 4` only when `model_transport::transient(error, diagnostics.output_started())` returns `Some`; it does NOT touch `w.messages` (since `w.messages.push(stored)` is at `:2460`).
   - Note on `awaiting_async` (`:2437-2439`): if `no_calls` and visual/plugin jobs are pending, `model_round` logs `"assistant/draft"` and returns early without pushing to `w.messages`.

2. **`end_familiarization` (`src/single_agent/program_flow.rs:796-838`, called from `submit_program` at `src/single_agent.rs:4289`)**:
   - Finds `start = w.messages.iter().rposition(|m| is_user_original(m) && m.get("name").is_none())` and `split_off(start + 1)`.
   - Keeps ONLY: `is_user_original`, `program_result | program_record`, `DELIVERED | modules::UPDATE | ADDED_TOOLS`, `program_diagnostic` (converted to `program_record`), and `_pinned == true && role == "tool"` (converted to `role: "user", name: "saved_asset"`, removing `tool_call_id`).
   - **Drops `virtual_step_handoff` (`name == "virtual_step_handoff"`)** whenever `handoff_step` ran after `start` (plus drops `virtual_step_wrap_required`, `execution_plan`, `execution_ledger`, `task_asset_index`, and any `role: "user", name: "saved_asset"` that was ALREADY converted by an earlier `reset_messages_for_virtual_step` or `fresh_main_window` because `:828` requires `message["role"] == "tool"`!).

3. **`reset_messages_for_virtual_step` (`src/single_agent.rs:1207-1240`, called on `handoff_step` at `:4056`)**:
   - Computes `prefix_end = position(is_user_original)` (which matches the first `is_user_original` message, e.g., turn 1's user prompt or an earlier `saved_asset`? Wait: `is_user_original` matches `role == "user" && name.is_none()` — let's verify in `task_frames.rs`, note `saved_asset` has `name: "saved_asset"`, `virtual_step_handoff` has `name: "virtual_step_handoff"`).
   - Before `prefix_end`: keeps messages except `name == VIRTUAL_STEP_HANDOFF` (`:1215-1219`).
   - At/after `prefix_end`: keeps ONLY `is_user_original(&message)` or `message["_pinned"] == true` (converting `role == "tool"` to `user`/`saved_asset`).
   - **Drops** after `prefix_end`: transient tool calls/results, `virtual_step_wrap_required`, `task_asset_index`, AND note it also drops non-`_pinned` `program_result`, `program_record`, `DELIVERED`, `modules::UPDATE`, `ADDED_TOOLS` if any appeared after `prefix_end`.
   - Appends `virtual_step_handoff_message(w, Some((&handoff.next_step, &handoff.next_goal)), false)` (`:1234-1238`).

4. **`clear_failed_program_attempts` (`src/single_agent/program_flow.rs:842-880`, called at `src/single_agent.rs:4286`)**:
   - In child `Implement` role, `split_off(start + 1)` from last `_frame_input == true`, dropping failed `write_program` calls/results, `program_diagnostic`, `execution_ledger`, `execution_plan`, while keeping `_pinned`, `DELIVERED`, `ADDED_TOOLS`, etc.

5. **Turn boundary & frame transitions**:
   - `fresh_main_window` (`src/single_agent/task_frames.rs:838-898`, called from `run_task_with_resource` at `src/single_agent.rs:4987`): rebuilds main window from `chat_context`, preserves `main_context`, `step_trail`, `context_slices`, `workspace_ledger`, pinned `active_instruction_ids` as `saved_asset`, and appends `virtual_step_handoff_message(&window, None, true)`.
   - `run_task_with_resource` (`src/single_agent.rs:4995-5000`): `w.messages.retain(...)` strips `task_return_required` / `task_completion_check`.
   - `task_frames::finish` (`src/single_agent/task_frames.rs:520-643`, called at `src/single_agent.rs:3846, 4097`) & `run_rounds` pause-switch (`src/single_agent.rs:3603-3615`): replaces `*w` with parent/main window.
   - `deliver_interjections` / `deliver_visual_jobs` (`src/single_agent.rs:3368-3393, 3443-3504`): clones `w` to `next`, extends `next.messages`, overwrites `*w = next`.

---

## 3. Exact Lines & Analysis for the 4 Fixes

### Fix 1: `end_familiarization` (`src/single_agent/program_flow.rs:796-838`)
- **Lines**: `src/single_agent/program_flow.rs:796-838`.
- **Single caller**: `src/single_agent.rs:4289` inside `submit_program` (`else if !body && s.plugin.is_none()`).
- **Existing unit tests**:
  - `src/single_agent/program_flow.rs:4600-4619` (`what_the_host_delivered_stays_when_a_program_starts`)
  - `src/single_agent/program_flow.rs:4622-4637` (`end_familiarization_and_implement_clear_stale_ledgers_plans_and_errors`)
- **Exact gaps in `796-838`**:
  - Line `807`: `matches!(message["name"].as_str(), Some("program_result" | "program_record"))` does NOT include `super::VIRTUAL_STEP_HANDOFF` (`"virtual_step_handoff"`), so any prior `handoff_step` frame (`mainContext`, `context_slices`, `workspace_ledger`, `current_step`) is discarded when `write_program` runs!
  - Line `828`: `else if message["_pinned"] == true && message["role"] == "tool"` misses messages where `message["_pinned"] == true` was ALREADY converted to `role == "user", name == "saved_asset"` by `reset_messages_for_virtual_step` (`src/single_agent.rs:1223-1231`) or `fresh_main_window` (`src/single_agent/task_frames.rs:871-886`)!
  - Note: if `virtual_step_handoff` is kept (or re-emitted), also check if `start` (`:797`) is before `virtual_step_handoff` (it always is, since `virtual_step_handoff` has `name: "virtual_step_handoff"` and is pushed after the user message).

### Fix 2: `event_seq` Injection on Tool Delivery (`src/single_agent.rs:3895-3930`)
- **Exact lines (`src/single_agent.rs:3895-3925`)**:
  - `:3895-3902`: `delivery` is `result.clone()` for `read_asset`, otherwise `crate::context_assets::delivered(&result, reference)` (if `asset_reference` is `Some`), else `result.clone()`.
  - `:3905-3913`:
    ```rust
    let library_result = crate::agent_program::libraries::public_name(
        &program_flow::library_catalog(s, &definitions),
        &call.name,
    )
    .is_some()
        || matches!(call.name.as_str(), program_flow::FIND | program_flow::ASK);
    if !library_result && let Some(object) = delivery.as_object_mut() {
        object.entry("event_seq").or_insert(json!(seq));
    }
    ```
  - `:3918-3920`: `if asset_reference.is_some() { delivery["delivery_reference"] = json!({"task_id":s.task,"event_seq":seq}); }`
- **Does `3895-3930` run for JS `require(...)` or JS program tool calls?**
  - **NO.** Verified in `src/single_agent/program_flow.rs`:
    - `ActionKind::Require { library }` (`program_flow.rs:2147-2169`) calls `link(...)`, logs `"program/require"`, calls `w.program.as_mut().unwrap().resume(value, failed)`, and returns `Ok(Round::Continue)` — never touching `run_rounds` lines `3870-3930`.
    - `ActionKind::Tool` / `ActionKind::ParallelReads` (`program_flow.rs:2145, 2170, 2200-2225`) calls `execute_call(...)` (`:2200`), then `resolve(s, w, &action, result, is_error, done.seq)` (`:2223`), and returns `Ok(Round::Continue)` (`:2225`) — also never touching `single_agent.rs:3895-3930`.
    - Only direct model tool calls (`model_round` -> `run_rounds` `:3870-3930`) execute lines `3895-3930`!
- **Tests checking tool output JSON keys / `event_seq`**:
  - `src/single_agent.rs:7678`: `let seq = payload["event_seq"].as_i64().unwrap();` (explicitly expects `event_seq` on `read_file_lines` delivery!).
  - `src/single_agent.rs:6484-6488`: checks `result["analysis"]` and `result["status"]` after unwrapping `original_result`.
  - Note for `read_asset`: at `:3895`, `read_asset` skips `delivered(...)`, and then `:3911-3913` inserts `"event_seq": seq` (the `read_asset` call's own seq) unless `result` already had `"event_seq"` (`:3187`), while `:3919` sets `delivery_reference.event_seq = seq`.

### Fix 3: `handoff_step` & Slice / Virtual Step Handling (`src/single_agent.rs:550-1245`)
- `handoff_step_tool` schema: `src/single_agent.rs:535-575` (`slices` `maxItems: MAX_HANDOFF_SELECTORS_PER_CALL`, `max_chars` max `MAX_HANDOFF_SLICE_ITEM_CHARS`).
- `build_slice_entry`: `src/single_agent.rs:677-838`:
  - Calls `crate::context_assets::read_event_payload(root, task, seq)` at `:682`.
  - Branches on: `keys` (`:703-725`), `path` + `lines|content` (`:727-763`), `exit_code` + `stdout|stderr` (`:765-788`), `matches|files` (`:790-821`), or generic pretty-printed JSON fallback (`:823-837`).
  - **Gap**: If `read_event_payload` returns a `read_asset` result (`{"asset_reference": ..., "original_result": {...}}` or paged `{"content": ...}`), `build_slice_entry` does NOT unwrap `original_result` (unless `read_event_payload` unwraps it first).
- `attach_handoff_context`: `src/single_agent.rs:1120-1151` (runs `build_slice_entry`, `merge_handoff_slices`, `build_workspace_ledger`).
- `virtual_step_handoff_message`: `src/single_agent.rs:1153-1205` (builds `VIRTUAL_STEP_HANDOFF` user message with `previous_steps`, `mainContext`, `workspace_ledger`, `context_slices`, `current_step` / `carried_across_turns`).
- `reset_messages_for_virtual_step`: `src/single_agent.rs:1207-1240` (drains `w.messages`, keeps `< prefix_end` except `VIRTUAL_STEP_HANDOFF`, keeps `is_user_original` and `_pinned == true`, appends new `virtual_step_handoff_message`).

### Fix 4: `context_assets.rs` (`read_event_payload` & `frame_index`)
- **`register` (`src/context_assets.rs:180-255`)**:
  - `is_material_tool` (`:170-201`) + `register` (`:205-260+`): computes `asset-{hash}`, stores snapshot in `.codex-workspace-mcp/assets/{payload_hash}.json`, persists in `agent_context_assets` (`history_reference.event_seq = seq`).
- **`read_event_payload` (`src/context_assets.rs:538-597`)**:
  - `:539-550`: looks up `asset_id_for_seq(root, task, seq)` (`agent_context_assets` where `history_reference.event_seq = seq`, or `agent_context_asset_deliveries` where `event_seq = seq` — note `agent_context_asset_deliveries` has no `INSERT` calls in `context_assets.rs`), reads `snapshot_path`.
  - `:551-575`: falls back to `agent_task_events` (`meta.result` or `message.content[0].text`).
  - `:576-595`: resolves `source_tool` from `flow/ledger` (`tool_result_seq = seq`) or `tool/call` (`callId`).
  - **Gap for `read_asset` events**: When `seq` belongs to a `read_asset` `tool/result` event (`source_tool == Some("read_asset")`), `asset_id_for_seq` returns `None`, so it reads `meta.result` directly:
    - Full `read_asset` returns `{"asset_reference": {"asset_id": ..., "source_tool": ..., "history_reference": {"event_seq": orig_seq}}, "original_result": {...}, ...}`.
    - Paged `read_asset` returns `{"asset_reference": ..., "content": "...", "char_offset": ..., ...}` (no `original_result`).
    - Unwrapping in `read_event_payload` (e.g., if `source_tool == "read_asset"` and `payload["original_result"]` is present, return `payload["original_result"]` with underlying `asset_reference["source_tool"]`, or follow `asset_reference["asset_id"]` / `history_reference["event_seq"]` to the full snapshot on disk!) lets `build_slice_entry` slice paged or full `read_asset` events cleanly by `seq`.
- **`frame_index` (`src/context_assets.rs:851-961`)**:
  - `:929`: `let mut item = json!({"delivery_reference":{"task_id":task,"event_seq":seq}});`
  - **Gap**: `seq` is only nested at `delivery_reference.event_seq`, not exposed as top-level `"seq": seq` (or `"event_seq": seq`) on `task_asset_index` entries (`assets[]`).
  - **Limits**: `:889-890` `LIMIT 65`, `:928` `.take(64)`, `:950` `if tokens + size > 3000 { break; }`.
