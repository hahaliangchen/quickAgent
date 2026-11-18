# Deep Comparison: `task_ae297abc...` vs Baseline `task_eecc4be8...`

## 0. Task Metadata & High-Level Comparison

| Metric | `task_ae297abc3ef0a5398b90724de6dda7a4` (`ae29`) | `task_eecc4be83c44ad10eb71e045cae95be9` (`eecc`, 16m09s baseline) |
|---|---|---|
| **Status** | `cancelled` (aborted mid-turn at Step 49) | `completed` (finished cleanly at Step 28) |
| **Model** | `gpt-6.1-terra` | `gpt-6.1-terra` |
| **Duration** | `1462.3s` (`24m22s`, `1794835578852 -> 1794837041106`) | `968.7s` (`16m09s`, `1794802712274 -> 1794803681013`) |
| **`event_high_water`** | `1153` | `1594` |
| **Request Steps (`agent_request_contexts`)** | `33` model requests spanning `step 1..49` (steps `17-23`, `29-32`, `40-44` inside JS programs) | `28` model requests spanning `step 1..28` |
| **`flow/step_wrap_requested` / `flow/step_handoff`** | **9** wraps / **9** handoffs (5 pre-`write_program`, 4 post-`write_program`) | **4** wraps / **4** handoffs (Steps 4, 7, 10, 13; 0 in Steps 14–28) |
| **`write_program` executions** | **3** (`step 16, 28, 39` — all 3 blocked on `/tmp` < 20 GiB disk gate) | **0** (ran 5 narrow Cargo checks via `run_readonly_command`) |
| **Total `tool/call` count** | `130` (`59` pre-prog in Steps 1–15; `71` post-prog in Steps 16–49) | `131` |
| **`mainContext` size trajectory** | `621 -> 877 -> 1022 -> 1456 -> 1890 -> 2086 -> 2289 -> 2614 -> 2887` B | `2729 -> 5301 -> 11412 -> 12500` B |
| **`context_slices` count & usability** | `0 -> 5 -> 5 -> 5 -> 5 -> 5 -> 5 -> 12 -> 13` (**0/13 valid excerpts**) | N/A (not present in baseline schema) |

---

## 1. Every `flow/step_wrap_requested` and `flow/step_handoff` Event

### 1A. `task_ae297abc3ef0a5398b90724de6dda7a4` (`ae29`) — 9 Wraps & 9 Handoffs

| # | Event | `seq` | Time | `step` | `estimatedTokens` | `roundAddedChars` | `completedStep -> nextStep` | `mainCtxBytes` | `sliceCount` | `updatedKeys` |
|---|---|---|---|---|---|---|---|---|---|---|
| W1 | `step_wrap_requested` | 68 | +41.5s | 2 | 112,529 | 193,737 | (roundsInScope=2) | — | — | — |
| **H1** | `step_handoff` | 93 | +76.6s | 3 | — | — | `inventory_and_scope` -> `targeted_evidence_and_minimal_checks` | 621 | 0 | `dirty_paths`, `docs_skill_persistence`, `docs_virtual_ledger`, `head`, `log_count`, `source_tree_claims_to_verify`, `target_log_dates`, `user_claimed_head` |
| W2 | `step_wrap_requested` | 176 | +170.7s | 6 | 149,187 | 262,934 | (roundsInScope=3) | — | — | — |
| **H2** | `step_handoff` | 223 | +226.4s | 7 | — | — | `inventory_and_scope` -> `read_targeted_evidence` | 877 | 5 | `actual_head`, `candidate_tests`, `dated_log_files`, `dirty_paths`, `history_after_claimed_head`, `unrelated_log_files`, `user_claimed_head` |
| W3 | `step_wrap_requested` | 255 | +260.5s | 8 | 66,048 | 132,058 | (roundsInScope=1) | — | — | — |
| **H3** | `step_handoff` | 275 | +289.5s | 9 | — | — | `targeted_evidence_round` -> `read_logs_and_source_units` | 1,022 | 5 | `actual_head`, `dated_log_files`, `docs_skill_persistence`, `source_claims`, `tests_not_run`, `unrelated_log_files` |
| W4 | `step_wrap_requested` | 328 | +349.1s | 11 | 140,000 | 300,734 | (roundsInScope=2) | — | — | — |
| **H4** | `step_handoff` | 364 | +416.4s | 12 | — | — | `inventoryAndScope` -> `auditAndVerify` | 1,456 | 5 | `actual_head`, `dated_log_classification`, `dated_logs_relevant`, `docs_skill_persistence`, `docs_virtual_ledger`, `source_files_confirmed`, `tests_not_run` |
| W5 | `step_wrap_requested` | 423 | +462.3s | 14 | 87,248 | 166,663 | (roundsInScope=2) | — | — | — |
| **H5** | `step_handoff` | 462 | +528.2s | 15 | — | — | `lockTargetsAndRunMinimalChecks` -> `runMinimalTargetedTests` | 1,890 | 5 | `actual_head`, `candidate_tests`, `dated_log_classification`, `docs_skill_persistence`, `docs_virtual_ledger`, `tests_not_run`, `user_claimed_head` |
| *P1* | `program/start..end` | 542..564 | +637.3s | 16 | — | — | `run-targeted-mainline-audit.js` (blocked: `fields[2]` `Used`=0 & `/tmp` < 20 GiB) | — | — | — |
| W6 | `step_wrap_requested` | 616 | +705.9s | 25 | 119,582 | 217,371 | (roundsInScope=2) | — | — | — |
| **H6** | `step_handoff` | 651 | +747.2s | 26 | — | — | `inventoryWorkspaceAndEvidence` -> `targetedEvidenceReview` | 2,086 | 5 | `cargo_recheck_needed`, `comparison_base`, `dirty_paths`, `head`, `previous_targeted_run`, `previous_targeted_run_valid` |
| *P2* | `program/start..end` | 739..752 | +889.8s | 28 | — | — | `run-mainline-audit-checks.js` (blocked: `df -Pk ... /tmp` min=14,935,004 KiB < 20 GiB) | — | — | — |
| W7 | `step_wrap_requested` | 811 | +951.9s | 34 | 129,509 | 240,140 | (roundsInScope=2) | — | — | — |
| **H7** | `step_handoff` | 834 | +1015.6s | 35 | — | — | `inventory_and_blocker_check` -> `focused_audit` | 2,289 | 5 | `build_dirs_cleaned`, `dirty_count`, `disk_available_kib`, `head`, `head_claim_from_user`, `prior_targeted_checks_blocked`, `source_docs_untouched`, `target_log_2026_11_14_count`, `target_log_dates` |
| W8 | `step_wrap_requested` | 876 | +1050.3s | 36 | 123,971 | 343,716 | (roundsInScope=1) | — | — | — |
| **H8** | `step_handoff` | 922 | +1112.8s | 37 | — | — | `evidence_reconciliation` -> `targeted_verification_and_audit` | 2,614 | 12 | `build_dirs_cleaned`, `dated_logs_relevant`, `log_count`, `next_stage`, `source_docs_untouched`, `tests` |
| *P3* | `program/start..end` | 1014..1031 | +1321.1s | 39 | — | — | `run-narrow-mainline-audit-checks.js` (blocked: `df -Pk ... /tmp` = 14,935,004 KiB) | — | — | — |
| W9 | `step_wrap_requested` | 1092 | +1384.3s | 46 | 90,957 | 133,695 | (roundsInScope=2) | — | — | — |
| **H9** | `step_handoff` | 1125 | +1434.2s | 47 | — | — | `inventory_and_scope` -> `evidence_matrix` | 2,887 | 13 | `commits_after_claimed_head`, `dirty_paths`, `head`, `narrow_checks_status`, `source_log_date_range_seen`, `source_log_relevance`, `user_claimed_head` |

### 1B. `task_eecc4be83c44ad10eb71e045cae95be9` (`eecc`) — 4 Wraps & 4 Handoffs

| # | Event | `seq` | Time | `step` | `estimatedTokens` | `roundAddedChars` | `completedStep -> nextStep` | `mainCtxBytes` | `sliceCount` | `updatedKeys` |
|---|---|---|---|---|---|---|---|---|---|---|
| W1 | `step_wrap_requested` | 114 | +95.0s | 3 | 290,095 | 282,536 | (roundsInScope=3) | — | — | — |
| **H1** | `step_handoff` | 175 | +149.0s | 4 | — | — | `inventory_and_premise_check` -> `audit_sources_and_logs` | 2,729 | None | `actual_head`, `constraints`, `current_status`, `design_doc_facts`, `head_relation`, `log_inventory`, `named_paths_status`, `user_claimed_head` |
| W2 | `step_wrap_requested` | 251 | +195.3s | 6 | 86,592 | 154,119 | (roundsInScope=2) | — | — | — |
| **H2** | `step_handoff` | 317 | +243.8s | 7 | — | — | `audit_sources_and_logs` -> `classify_logs_and_tests` | 5,301 | None | `actual_head`, `candidate_logs`, `dirty_paths`, `doc_facts`, `named_paths_clean`, `recent_logs_already_known`, `source_facts` |
| W3 | `step_wrap_requested` | 434 | +299.3s | 9 | 109,927 | 218,387 | (roundsInScope=2) | — | — | — |
| **H3** | `step_handoff` | 533 | +360.1s | 10 | — | — | `classify_logs_and_tests` -> `retest_narrow_current` | 11,412 | None | `audit_hypotheses`, `constraints_reiterated`, `current_retest_candidates`, `doc_alignment`, `log_evidence`, `source_evidence` |
| W4 | `step_wrap_requested` | 597 | +392.4s | 12 | 60,054 | 162,817 | (roundsInScope=2) | — | — | — |
| **H4** | `step_handoff` | 636 | +422.5s | 13 | — | — | `lock_narrow_retest_targets` -> `run_narrow_current_retest` | 12,500 | None | `known_old_logs`, `next_run_rules`, `retest_targets`, `source_evidence`, `test_scope` |

---

## 2. Step-by-Step Timeline Comparison

### `ae29` (Steps 1–15 Pre-Program & Steps 16–49 Post-Program)
- **Steps 1–3 (H1 `inventory_and_scope`)**: `workspace_info`, `list_dir(target **/*.log)`, `list_skills`, 3 git commands, 2 `search_text` (`docs`, `target`), `list_dir(target depth=2)`, 2 `read_skill`. `roundAddedChars=193,737` -> wrap -> H1 (`mainCtx=621` B, `slices` omitted).
- **Steps 4–7 (H2 `inventory_and_scope`)**: Restored 3 assets via `read_asset` (57, 53, 49), then **re-ran** `list_dir(target)`, `git status/HEAD/diff`, `search_text` on `src/` & `docs/`, plus broad `search_text` across `target/**/*.log` & `src/*.rs` + `git log --all` (`262,934` chars in round 6) -> wrap -> H2 (`mainCtx=877` B, passed 5 `slices`, 4 empty + 1 truncated JSON wrapper). Still hadn't read full log contents/source units, so named Step 7 `inventory_and_scope` again.
- **Steps 8–9 (H3 `targeted_evidence_round`)**: Because `slices` were 4/5 empty and `mainCtx` held only path/flag keys, **re-ran** `workspace_info`, `list_dir(target)`, and 3 `search_text` (`docs`, `flow_libraries.rs`, multi-file `src/`) in Step 8 -> `132,058` chars in 1 round (`roundsInScope=1`) -> immediate wrap -> H3 (`mainCtx=1,022` B, `slices` omitted).
- **Steps 10–12 (H4 `inventoryAndScope`)**: Called `read_asset` x4 (seq `103..` era assets), then in Step 11 **re-ran** `git status/HEAD/ls-files`, `search_text(target)`, `search_text(src)`, `search_text(docs)` -> `300,734` chars -> wrap -> H4 (`mainCtx=1,456` B, `slices` omitted). Still doing inventory/scoping, so named Step 12 `inventoryAndScope`.
- **Steps 13–15 (H5 `lockTargetsAndRunMinimalChecks`)**: **Re-ran** `workspace_info`, `search_text(docs)`, `search_text(src)`, `df -h`, then `read_file_lines` (`flow_libraries.rs`, `workflow_drafts.rs`, `Cargo.toml`) + `search_text` -> `166,663` chars -> wrap -> H5 (`mainCtx=1,890` B).
- **Step 16 (`write_program` #1)**: `run-targeted-mainline-audit.js` blocked before Cargo because `parseAvailableBytes` read `df` column `fields[2]` (`Used=0` on `/tmp`) AND checked `/tmp` (`14,935,004` KiB < 20 GiB).
- **Steps 24–28 (`write_program` #2 after H6)**: After `write_program` #1 returned, Step 24–25 **re-ran** `workspace_info`, `list_dir(target)` x2, `git status/HEAD/diff` x2, `search_text` x3 (`217,371` chars) -> H6 -> `read_asset` x3 -> Step 28 `write_program` #2 (`run-mainline-audit-checks.js`). Fixed `awk` to `$4`, but STILL passed `/tmp` to `df -Pk`, hitting `exit 125` (`14,935,004 < 20,971,520`).
- **Steps 33–39 (`write_program` #3 after H7 & H8)**: Steps 33–34 **re-ran** `workspace_info`, `git status/HEAD`, `list_dir(target)` x2, `git diff`, `search_text` x2 -> H7 (`slices: []`) -> Step 36 `read_asset` x3 + `list_dir(target)` + `read_file_lines` x4 -> `343,716` chars in 1 round -> H8 (`slices` added 7 items, 4 empty + 3 truncated `read_asset` envelopes) -> Step 38 `search_text` x3 + `df -Pk` -> Step 39 `write_program` #3 (`run-narrow-mainline-audit-checks.js`, STILL checked `/tmp`!).
- **Steps 45–49 (H9 & `cancelled`)**: Step 45 `read_asset` x5 -> Step 46 **re-ran** `workspace_info`, `list_dir(target)`, `git status/HEAD`, `git diff`, `search_text` x2 -> wrap -> H9 (`slice` on `set_next_step` seq 1090 -> `"1: {"`) -> Step 48 `read_asset` x4 + `set_next_step` -> Step 49 `cancelled`.

### `eecc` (Steps 1–28)
- **Steps 1–13**: 4 wraps/handoffs (`step 4, 7, 10, 13`), each strictly advancing stage (`inventory_and_premise_check` -> `audit_sources_and_logs` -> `classify_logs_and_tests` -> `lock_narrow_retest_targets` -> `run_narrow_current_retest`) while distilling **12,500 B** into `mainContext`.
- **Steps 14–28**: **0 handoffs** for 15 consecutive steps (`prompt_tokens` stayed `19.5k -> 50.1k`, never crossing 120k tokens or 120k round chars). Used `run_readonly_command` directly for narrow Cargo tests, inspected small status/log files via `read_file`/`read_file_lines`, and completed cleanly at Step 28 (`5,692` completion tokens).

---

## 3. Root-Cause Breakdown (`agent_request_contexts`, `src/single_agent.rs`, `prompts/single_agent.md`)

### Root Cause 1: Prompt/Schema Shift Shrunk `mainContext` from 12.5 KB (`eecc`) to 2.9 KB (`ae29`)
- In `git diff baseline-16m9s -- prompts/single_agent.md src/single_agent.rs`:
  - **Baseline (`eecc`)** instructed `handoff_step` to *"merge compact verified facts/paths/commands (plus exact `path:line_range` / line counts...) into shared `mainContext` (`main_context_patch`, <= 32 KiB; total <= 64 KiB)"*. `eecc` stored full verified matrices in `mainContext` (`log_evidence` 2,448 B, `log_inventory` 1,191 B, `audit_hypotheses` 744 B, `current_retest_candidates` 706 B, `doc_alignment` 668 B, `source_facts` 657 B, etc., totaling `12,500` B across 24 keys).
  - **Current (`ae29`)** changed `prompts/single_agent.md` & `handoff_step_tool()` (`src/single_agent.rs:568-570`) to: *"keep `main_context_patch` strictly to tiny structured keys/arrays ... never write long summaries or raw dumps"*. `ae29` complied: `mainContext` only reached `2,887` B (mostly path arrays, booleans like `dated_logs_relevant: false`, and test names), offloading detail onto `situation` (1–2 sentences) and `slices`.

### Root Cause 2: `context_slices` Completely Failed in `ae29` (**0 / 13 Usable Excerpts**)
Inspecting `agent_request_contexts` (`step=8, 38, 48`) shows why every slice passed by `ae29` failed:
- **Handoffs #1, #3, #4, #5, #6**: `slices` omitted (`null`).
- **Handoff #2 (`step 7`)**: Passed 5 slices — **4 returned `excerpt: ""`**, **1 returned a `4000`-char `clipped: true` JSON envelope**:
  - `seq=124` (x2) and `seq=154` (x2) were **`set_next_step`** tool results, NOT `list_dir`/`run_readonly_command`/`search_text`! Grep found 0 matches -> `excerpt: ""`.
  - `seq=112` was a **`read_asset`** wrapper result, not the underlying `search_text` event `seq=49` -> sliced lines `[28, 32]` of the JSON wrapper (`"char_offset": 0, "content": "{\\"index_matches\\":0..."`), hitting the `4000`-char clip (`DEFAULT_HANDOFF_SLICE_ITEM_CHARS`).
- **Handoff #7 (`step 35`)**: Passed `slices: []` without `replace_slices: true`, so the 5 broken slices from H2 remained pinned through H2–H7.
- **Handoff #8 (`step 37`)**: Appended 7 more slices (`sliceCount=12`):
  - `seq=848` (x2, `read_asset`) & `seq=851` (x1, `read_asset`): `clipped: true` `4000`-char JSON envelopes.
  - `seq=854` (x3, `read_asset` with `lines: [141, 162]`, `[60, 71]`, `[108, 117]`): the `read_asset` JSON envelope only has ~32 pretty-printed lines (`"content"` is one escaped string line), so `lines >= 60` matched **0 lines** -> **`excerpt: ""`** on all 3!
  - `seq=845` (x1, `set_next_step`, `keys: ["workspace_ledger", "mainContext", "current_step"]`): returned `data: {"current_step": "evidence_reconciliation"}` (missing `workspace_ledger` and `mainContext`).
- **Handoff #9 (`step 47`)**: Appended `seq=1090` (`set_next_step`, `head: 1`) -> `excerpt: "1: {"` (4 bytes), bringing `sliceCount=13`.

**Why did the model keep passing `set_next_step` and `read_asset` `seq`s instead of real tool `seq`s?**
A bug in `src/single_agent.rs:3905-3913`:
```rust
let library_result = crate::agent_program::libraries::public_name(
    &program_flow::library_catalog(s, &definitions),
    &call.name,
).is_some() || matches!(call.name.as_str(), program_flow::FIND | program_flow::ASK);
if !library_result && let Some(object) = delivery.as_object_mut() {
    object.entry("event_seq").or_insert(json!(seq));
}
```
Because `library_catalog` includes all JS-backed workspace tools (`list_dir`, `search_text`, `run_readonly_command`, `read_file*`, `workspace_info`), `library_result` is `true` for ALL of them, so **inline workspace tool outputs NEVER receive `"event_seq"`** (verified in `ae29` Step 6 `msg[17..20]`: `has_event_seq=False`). Only `set_next_step` (excluded in `libraries.rs:294`), `read_asset` (`history_reference.event_seq`), and overflowed `task_asset_index` entries expose an `event_seq` to the model!

### Root Cause 3: `virtual_step_handoff_message` + `workspace_ledger` Information Loss Between Virtual Steps
- Does `step_trail` drop prior steps? **No**: `virtual_step_handoff_message` (`src/single_agent.rs:1158-1171`) emits all prior steps in `previous_steps` (`index`, `step`, `situation`, `purpose`, `updated_keys`).
- Does `handoff_step` without rich `mainContext` cause severe information loss? **Yes**:
  1. `reset_messages_for_virtual_step` (`src/single_agent.rs:1207-1240`) drops all non-pinned tool outputs and assistant messages.
  2. `situation` is capped by prompt guidance to 1–2 concise sentences (`275–531` chars in `ae29` vs `464–1259` chars in `eecc`).
  3. `build_workspace_ledger` (`src/single_agent.rs:896-1040`) ONLY tracks file read/edit tools (`files`, max 40) and `run_readonly_command`/`exec_command` (`commands`, max 12, cmd truncated to 80 chars, **no stdout/stderr**). It ignores `search_text`, `list_dir`, `workspace_info`, and `read_asset`. In `ae29` Handoffs #1–#4, **`workspace_ledger.files = 0`**; in H5–H9 it had only 3–7 files and zero `search_text`/`git` outputs.
  4. Combined with `0/13` valid `context_slices` and skeletal `mainContext`, each new virtual step woke up nearly blind to prior `git`/`target`/`search_text` findings and immediately re-queried them, hitting `STEP_WRAP_ROUND_CHARS` (`120_000`) again in 1–3 rounds.

---

## 4. Exact Counts of Repeated Tools / Files / Commands

| Tool / Target | `ae29` Pre-Prog (Steps 1–15) | `ae29` Post-Prog (Steps 16–49) | `ae29` Total (Steps 1–49) | `eecc` Total (Steps 1–28) |
|---|---|---|---|---|
| **All tool calls** | `59` | `71` | `130` | `131` |
| `workspace_info` (`{}`) | **3** | **3** | **6** (5 extra repeats) | **4** |
| `list_dir` | **4** (`target **/*.log` x3, `target` depth=2 x1) | **6** (`target *.log` x3, `target` depth=2 x1, `target **/*.log` x1, `target/investigations` x1) | **10** (9 on `target/`) | **18** (diverse dirs: `src`, `docs`, `target/investigations/...`) |
| `run_readonly_command` | **7** (git status: 3, git diff: 4, git rev/log/show: 5, `df`: 1) | **15** (git status: 5, git diff: 3, git rev/log/show: 4, `df`: 6, mkdir/rm: 4, empty-preview wrapper cmds: 3) | **22** (`git status`: **8**, `git diff`: **7**, `git rev/log/show`: **9**, `df`: **7**) | **10** (`git status`: 7, `git rev/log`: 9, `git diff`: 2, `df`: 2, cargo wrappers: 5) |
| `search_text` | **19** (`docs/agent-design-guide.md`: **5**, `target/`: **3**, `src/` & `.rs`: **11**) | **10** (`docs/agent-design-guide.md`: **3**, `target/`: **2**, `src/` & `.rs`: **5**) | **29** (`docs/agent-design-guide.md`: **8**, `target/`: **5**, `src/`: **16**) | **20** (each query distinct, progressive narrowing) |
| `read_asset` | **7** (7 unique assets) | **15** (2 assets read **2x** each: `asset-3c8d...` & `asset-6716...`; 1 failed `ASSET_NOT_FOUND` + re-read) | **22** (2 exact duplicate asset reads) | **0** |
| `read_file` / `read_file_lines` | **3** (`flow_libraries.rs`, `workflow_drafts.rs`, `Cargo.toml`) | **6** (`flow_libraries.rs` 1–700, `workflow_drafts.rs` 1–1152, `docs` 1–180, `single_agent.rs` 1–260, `status.json`, `disk-before-tests.txt`) | **9** (`flow_libraries.rs` x2, `workflow_drafts.rs` x2) | **60** (`30` `read_file` + `30` `read_file_lines`, mostly distinct logs/files) |
| `handoff_step` | **5** (Steps 3, 7, 9, 12, 15) | **4** (Steps 26, 35, 37, 47) | **9** | **4** (Steps 4, 7, 10, 13) |
| `set_next_step` | **8** | **8** | **16** | **11** |
| `write_program` | **1** (Step 16) | **2** (Steps 28, 39) | **3** | **0** |
