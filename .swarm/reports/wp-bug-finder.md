# Root-Cause Report: Post-`write_program` Handoff Vanishing & Empty `seq=124` Slice Excerpt

## 1. Why `virtual_step_handoff` and `saved_asset` Vanished After `write_program` (`task_ae297abc3ef0a5398b90724de6dda7a4`, `req_ctx` 2687 -> 2688 and 2692 -> 2693)

### Code Flow & Exact Citations
1. **Where `Window` messages are rebuilt when top-level `write_program` starts:**
   - `src/single_agent.rs:4287-4290`: When top-level `write_program` succeeds (`!body && s.plugin.is_none()`), the host calls:
     `program_flow::end_familiarization(w, message, source);`
   - `src/single_agent/program_flow.rs:796-838` (`end_familiarization`):
     - Line `797`: Finds `start` = the latest user message matching `is_user_original(m) && m.get("name").is_none()`.
     - Line `804`: Drains everything after `start` via `w.messages.split_off(start + 1)` and keeps **only** messages matching:
       - `task_frames::is_user_original(&message)` (`program_flow.rs:805`; `task_frames.rs:44-51` requires `name` to be `None` or `"user_interjection"` and no `asset_reference`)
       - `matches!(message["name"].as_str(), Some("program_result" | "program_record"))` (`program_flow.rs:807`)
       - `message.get(super::skill_scope::DELIVERED).is_some() || message["name"] == super::modules::UPDATE || message.get(crate::anthropic_messages::ADDED_TOOLS).is_some()` (`program_flow.rs:811-813`)
       - `message["name"] == "program_diagnostic"` (`program_flow.rs:821`, converted to `"program_record"`)
       - `message["_pinned"] == true && message["role"] == "tool"` (`program_flow.rs:828-835`, converted to `role="user", name="saved_asset"`)

2. **Where top-level `write_program` finishes (`program/result`):**
   - `src/single_agent/program_flow.rs:1891-1917` (`return_value`, branch `w.program_task.is_none()`):
     - Line `1899` calls `outcome(s, &output, Ok(&value))` (`program_flow.rs:1675-1740`, formatting `"{label}已完成。"` at `:1679,1682`).
     - Lines `1911-1916` clear `w.program = None; w.program_value = None;`, append `"program/result"`, push `{"role": "user", "name": "program_result", "content": text}` (`:1914`), call `save(s, w)` (`:1915`), and return `Ok(Round::Continue)` (`:1916`).
   - Importantly, `w.main_context`, `w.context_slices`, `w.workspace_ledger`, and `w.step_trail` (`src/single_agent.rs:129-136`) are **never cleared** by `end_familiarization` or `return_value`.

3. **Two bugs in `end_familiarization` (`src/single_agent/program_flow.rs:804-836`) caused the disappearance:**
   - **Bug 1A (`virtual_step_handoff` dropped):** `virtual_step_handoff_message` (`src/single_agent.rs:1153-1205`) emits `{"role": "user", "name": "virtual_step_handoff", "_frame_input": true, ...}` (`:1199-1204`). It matches none of the keep-branches in `end_familiarization` (`program_flow.rs:805-835`), so `end_familiarization` silently deletes it whenever top-level `write_program` is accepted.
   - **Bug 1B (pinned `saved_asset` skill refs dropped):** Prior `reset_messages_for_virtual_step` (`src/single_agent.rs:1223-1232`) and `fresh_main_window` (`src/single_agent/task_frames.rs:864-888`) already normalized `_pinned: true` skill tool messages into `{"role": "user", "name": "saved_asset", "_pinned": true, ...}`. Because `end_familiarization` (`program_flow.rs:828`) checks `message["_pinned"] == true && message["role"] == "tool"`, any already-normalized `saved_asset` fails `role == "tool"` and is also dropped.

4. **Live DB Verification (`task_ae297abc3ef0a5398b90724de6dda7a4`):**
   - `req_ctx 2687` (turn 1, step 16): `messages[6]`, `messages[7]` were `saved_asset` (events 61, 65); `messages[8]` was `virtual_step_handoff`.
   - `req_ctx 2688` (turn 1, step 24, after `write_program` #1 `"第 1 段程序（run-targeted-mainline-audit.js）已完成。"`): `messages[6..8]` (`saved_asset` + `virtual_step_handoff`) all vanished; only `execution_plan`, `write_program` call/tool, `program_result`, and `task_asset_index` remained.
   - Because `Window` retained `main_context` (31 keys), `step_trail` (8 entries), `context_slices` (12 entries), and `workspace_ledger` in `agent_runtime_windows`, a subsequent `handoff_step` (`src/single_agent.rs:4056`) re-emitted `virtual_step_handoff` in `req_ctx 2692` (step 28), and `write_program` #2 (`run-mainline-audit-checks.js`) stripped it again in `req_ctx 2693` (step 33).

---

## 2. Why Slicing `seq=124` (`tool: "set_next_step"`) Produced `"excerpt": ""`

### Code Flow & Live DB Verification
- In `task_ae297abc3ef0a5398b90724de6dda7a4`, event `seq=124` is `kind="tool/result"` for `set_next_step` (`meta.result` = `{"context_keys":[...],"current_step":...,"next_step":...,"recorded":true}`). In `handoff_step` (`seq=220`), the model mistakenly targeted `seq: 124` with `grep: "target/.*2026-11-(11|12|13)/.*\\.log"` and `grep: "^## master|^HEAD$|^b8a5ac1|^DIFF STAT|^DIFF NAMES"` (intended for `seq=127`/`131`).
- **`src/context_assets.rs:538-597` (`read_event_payload`):** `asset_id_for_seq` returns `None` (`:539`), so it falls back to `agent_task_events` (`:551-596`), returns `data.meta.result` (`:564-567`), and resolves `source_tool = Some("set_next_step")` from `flow/ledger` (`:576-583`).
- **`src/single_agent.rs:677-838` (`build_slice_entry`):**
  - `selector["keys"]` is unset (`:703`); `payload` lacks `path`+`lines`/`content` (`:727`), `exit_code` (`:765`), and `matches`/`files` (`:790`).
  - Execution falls through to the generic JSON fallback (`:823-837`), which pretty-prints `payload` (`11` lines) and passes it to `slice_numbered_lines(pairs, selector)` (`:594-675`).
  - In `slice_numbered_lines` (`:609-630`), `selector["grep"]` matches **0 lines** of `set_next_step`'s JSON, leaving `filtered` empty.
  - Lines `664-674` produce `range = None`, `display_lines = []`, `joined = ""`, `truncate_chars("", max_chars)` -> `("", false)`.
  - Lines `830-836` omit `sliced_range` and unconditionally write `"excerpt": ""` with no `status` / `matched: false` diagnostic, and `merge_handoff_slices` (`:850-870`) stores the empty slice into `w.context_slices` while `handoff_step` returns `"status": "handed_off"`.

---

## 3. Fix Design

1. **Preserve `saved_asset` and `virtual_step_handoff` across `end_familiarization` (`src/single_agent/program_flow.rs:796-838`):**
   - Keep any pinned message regardless of whether `role` is `"tool"` or `"user"` (`message["_pinned"] == true`), normalizing `role="tool"` to `role="user", name="saved_asset"` as before (`:828-835`).
   - Drop stale `VIRTUAL_STEP_HANDOFF` messages during the loop (like `reset_messages_for_virtual_step` at `src/single_agent.rs:1216`) and, at the end of `end_familiarization` (before or after `script`, or right before `w.messages.push(script)`), if `!w.main_context.is_empty() || !w.step_trail.is_empty() || !w.context_slices.is_empty() || !w.workspace_ledger.is_null()`, re-append `super::virtual_step_handoff_message(w, None, false)` (or pass `carried_across_turns = false` with an updated/clean step continuation note, ensuring it survives `write_program` start & `program_result`).
   - Also inspect `program_flow::end_familiarization` tests and `task_frames::fresh_main_window` (`task_frames.rs:889-896`, note it currently omits `!window.workspace_ledger.is_null()` — align if needed).

2. **Reject or flag empty/zero-match slices in `build_slice_entry` / `attach_handoff_context` (`src/single_agent.rs:677-838`, `:1120-1151`):**
   - In `build_slice_entry`:
     - Reject or mark non-substantive control-tool events (`matches!(source_tool.as_deref(), Some("set_next_step" | "handoff_step"))`) as `{"seq": seq, "tool": source_tool, "status": "non_evidence_event"}` (or skip persisting them).
     - Whenever `slice_numbered_lines` returns `range.is_none()` / `excerpt.is_empty()` (e.g., `grep` or `lines` matched 0 lines), return `status: "empty_match"` (do **not** silently store `"excerpt": ""` in `w.context_slices`), and surface `rejected_slices` / warnings in `handoff_step`'s tool `receipt` (`attach_handoff_context`, `src/single_agent.rs:1124,1147`) so the model immediately sees the wrong `seq` or `grep` instead of carrying empty slices across 8+ steps.
