//! Dedicated read-only Observer Agent configuration and capability isolation.
//!
//! Keeps observer-specific persona, protocol fences (`observer-suggestion` /
//! `blueprint-note`), and read-only host library filtering out of the core
//! `model_call` wire builder so the observer stays a pure, lightweight bystander
//! agent with global DB + workspace read visibility and zero write capabilities.

use crate::runtime::agent_program::{declarations, libraries::Libraries};
use std::path::Path;

/// Minimal JS-VM execution protocol for the Observer Agent.
/// Intentionally omits all coding, file-editing, shell, SSH, DB Studio, Browser,
/// plan-mutation, and Codex final-reply-layout instructions from `V2_SYSTEM_PROMPT`.
const OBSERVER_SYSTEM_PROMPT: &str = r#"You are a pure, read-only Observer Agent operating inside a per-turn JavaScript VM.
Your ONLY tool is `run_js({ code })`, which executes JavaScript statements against the strictly read-only host libraries declared in the `.d.ts` below and returns the real output in the `tool` response.

## EXECUTION PROTOCOL
Each response MUST be **one** of two actions:
1. **Call `run_js({ code })`**:
   - Emit raw, readable multi-line JavaScript statements (no markdown fences, no fabricated comments).
   - Batch independent read queries into one script (use `.promises` + `Promise.all` for concurrent reads) instead of dripping out one call per turn.
   - Top-level `const`/`let`/`function` bindings stay live across steps in the current turn; `io.read(...)` returns a `ResourceHandle` supporting `f.slice(start, end)`, `f.grep(regex, ctx?)`, `f.head(n)`, `f.tail(n)`.
   - Unconsumed result variables and bare expressions are returned automatically (avoid `console.log`).
   - Use `host.contracts({ libraries: [...] })` for full method schemas, and `session.read_asset(...)` / `session.snapshot(...)` for oversized host assets.
2. **Reply directly to the user (without calling `run_js`)**:
   - Ground every conclusion strictly in observed tool output; never fabricate tool results, line numbers, or citations."#;

/// Resolve the main task ID watched by `task_id` if it is a dedicated `"observer"` session.
pub(crate) fn watched_task(state_root: &Path, task_id: &str) -> Option<String> {
    crate::context::session_browser::observer_watched_task(state_root, task_id)
        .ok()
        .flatten()
}

/// Returns `true` when `task_id` (in a non-virtual scope) is a dedicated `"observer"` session.
pub(crate) fn is_observer_session(state_root: &Path, scope: &str, task_id: &str) -> bool {
    !scope.starts_with("virtual:") && watched_task(state_root, task_id).is_some()
}

/// Restrict `libs` to the strictly read-only capability subset required by an Observer Agent.
///
/// Enforced at VM library-binding time (`library_compile_scope`), so write/exec tools
/// (`io.edit`, `io.write`, `process.*`, `git.commit`, `blueprint.upsert_*`, external
/// write plugins, etc.) are neither exposed in `.d.ts` nor callable from `run_js`.
pub(crate) fn filter_libraries(libs: &mut Libraries) {
    libs.retain(|lib_name, methods| match lib_name.as_str() {
        "blueprint" => {
            methods.retain(|method, _| {
                matches!(
                    method.as_str(),
                    "overview"
                        | "open"
                        | "read_graph"
                        | "read_playbook"
                        | "inspect_session"
                        | "list_inbox"
                        | "impact"
                        | "verify_sync"
                )
            });
            !methods.is_empty()
        }
        "session" => {
            methods.retain(|method, _| {
                matches!(
                    method.as_str(),
                    "read_session_history" | "list_sessions" | "read_asset" | "snapshot"
                )
            });
            !methods.is_empty()
        }
        "io" => {
            methods.retain(|method, _| {
                matches!(
                    method.as_str(),
                    "workspace_info"
                        | "list"
                        | "stat"
                        | "read"
                        | "read_lines"
                        | "read_chunk"
                        | "search"
                )
            });
            !methods.is_empty()
        }
        "git" => {
            methods.retain(|method, _| {
                matches!(method.as_str(), "status" | "diff" | "log" | "blame")
            });
            !methods.is_empty()
        }
        "host" => {
            methods.retain(|method, _| {
                matches!(
                    method.as_str(),
                    "contracts" | "find_methods" | "guide" | "list_skills" | "read_skill"
                )
            });
            !methods.is_empty()
        }
        "attachments" => {
            methods.retain(|method, _| method == "read_attachment");
            !methods.is_empty()
        }
        _ => false,
    });
}

/// Assemble the pinned system prompt for an Observer Agent watching `target_task_id`.
pub(crate) fn build_system_prompt(target_task_id: &str, libs: &Libraries) -> String {
    let mut text = String::from(OBSERVER_SYSTEM_PROMPT);
    text.push_str(&format!(
        "\n\n=== OBSERVER AGENT ROLE ===\n\
你是专属于主会话 `{target_task_id}` 的独立只读旁观 Agent（能力层已严格隔离为只读），拥有对主会话全局 DB 与当前工作区的完整只读视野。\n\
- **先抓核心目标，区分主次**：先从主会话最新实质指令提炼本轮**真正的核心目标与关键验收项**（例如要验证的核心功能、关键指标、必须交付的产出），明确区分**核心目标**与**部署/切换/重试等中间准备步骤**——绝不把次要过程细节放大成本轮观察重点。\n\
- **轻量自主观察**：巡检优先调用一次 `blueprint.inspect_session({{ task_id: \"{target_task_id}\" }})` 掌握全局即可；仅在发现异常、明显卡死或确实需要核对代码事实时，再去调用 `session.read_session_history`、`blueprint.open({{}})` 或只读源码/Git 工具，正常推进时不要做耗时深挖。\n\
- **理解完整工程意图（拒绝机械抠字眼）**：按时间顺序整合全部 `user_instructions`（后续轮次覆盖早先冲突限制，简短续跑指令继承上一轮实质任务与授权）。对授权隔离环境内分阶段的可逆准备动作，按工程阶段语义理解，绝不做机械字面计数。\n\
- **默认安静，高阈值介入**：默认主会话正在正常推进；正常时用 1~3 句简明交代“围绕核心目标当前推进到哪一步、是否正常”即可，`should_interject: false`。仅在发现**真红线越界（触碰生产/禁改目标/不可逆破坏）、明显偏离核心目标、反复死循环空转或真实卡死**时才设 `should_interject: true`；对授权测试环境的可逆操作绝不拉 `blocker`/`stop`。若用户已忽略某条旧建议，尊重用户判断，不再重复提醒。\n\
- 每次回复末尾附精简 JSON 代码块供界面按需渲染确认卡片：\n\
```observer-suggestion\n\
{{\"should_interject\": false, \"action\": \"interject\", \"severity\": \"tip\", \"title\": \"\", \"reason\": \"当前推进正常\", \"interject\": \"\"}}\n\
```\n\
（`action` 可选 `\"interject\"` | `\"stop\"` | `\"stop_and_continue\"`；`severity` 可选 `\"tip\"` | `\"warn\"` | `\"blocker\"`。）\n\
- **会话结束沉淀蓝图（仅在任务完成或用户主动要求时附）**：进行中的计划阶段巡检不要附 `blueprint-note`；当主会话完成一轮实质研发/运维任务（或用户要求呈报蓝图）且确有可跨会话复用的业务模块、源码锚点、环境约束、红线或验收命令时，在末尾附一个 `blueprint-note` JSON 代码块呈报到蓝图收件箱（纯问答或无新沉淀则不附）：\n\
```blueprint-note\n\
{{\"title\": \"简明主题\", \"trigger\": \"何时需要参考这条经验\", \"summary\": \"涉及的业务类/模块关系与核心结论\", \"anchor_files\": [\"相对路径\"], \"environment\": [\"端口/路径/服务隔离约定\"], \"pitfalls\": [\"踩过的坑与红线\"], \"verify_commands\": [\"有效验收命令\"]}}\n\
```\n"
    ));
    text.push_str("\n/* === RUNTIME HOST SDK (.d.ts) === */\n");
    text.push_str(&declarations::static_library_header(libs));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn filter_libraries_keeps_strictly_read_only_observer_surface() {
        let mut libs = crate::runtime::agent_program::libraries::builtin();
        libs.insert(
            "blueprint".into(),
            BTreeMap::from([
                ("overview".into(), "blueprint__overview".into()),
                ("open".into(), "blueprint__open".into()),
                ("inspect_session".into(), "blueprint__inspect_session".into()),
                ("upsert_entities".into(), "blueprint__upsert_entities".into()),
                ("upsert_graph".into(), "blueprint__upsert_graph".into()),
                ("upsert_playbook".into(), "blueprint__upsert_playbook".into()),
                ("resolve_inbox".into(), "blueprint__resolve_inbox".into()),
            ]),
        );
        libs.insert(
            "ssh".into(),
            BTreeMap::from([("exec".into(), "ssh__exec".into())]),
        );

        filter_libraries(&mut libs);

        for excluded in ["process", "verify", "plan", "http", "browser", "sys", "vision", "ssh"] {
            assert!(
                !libs.contains_key(excluded),
                "observer libraries must exclude `{excluded}`"
            );
        }
        assert!(libs["blueprint"].contains_key("inspect_session"));
        assert!(libs["blueprint"].contains_key("open"));
        assert!(!libs["blueprint"].contains_key("upsert_entities"));
        assert!(!libs["blueprint"].contains_key("upsert_graph"));
        assert!(!libs["blueprint"].contains_key("upsert_playbook"));
        assert!(!libs["blueprint"].contains_key("resolve_inbox"));

        assert!(libs["io"].contains_key("read"));
        assert!(libs["io"].contains_key("search"));
        for write_method in ["write", "edit", "replace", "apply_edits", "delete", "rename", "mkdir", "copy"] {
            assert!(
                !libs["io"].contains_key(write_method),
                "observer `io` must exclude `{write_method}`"
            );
        }

        assert!(libs["git"].contains_key("status"));
        assert!(libs["git"].contains_key("diff"));
        assert!(!libs["git"].contains_key("commit"));
        assert!(!libs["git"].contains_key("restore"));

        assert!(libs["host"].contains_key("contracts"));
        assert!(!libs["host"].contains_key("ask_user"));

        assert!(libs["session"].contains_key("read_session_history"));
        assert!(libs["attachments"].contains_key("read_attachment"));
    }

    #[test]
    fn build_system_prompt_is_pure_without_coding_agent_bloat() {
        let mut libs = crate::runtime::agent_program::libraries::builtin();
        libs.insert(
            "blueprint".into(),
            BTreeMap::from([
                ("inspect_session".into(), "blueprint__inspect_session".into()),
                ("open".into(), "blueprint__open".into()),
            ]),
        );
        filter_libraries(&mut libs);

        let prompt = build_system_prompt("task_watched_123", &libs);
        assert!(prompt.starts_with(OBSERVER_SYSTEM_PROMPT));
        assert!(prompt.contains("=== OBSERVER AGENT ROLE ==="));
        assert!(prompt.contains("task_watched_123"));
        assert!(prompt.contains("```observer-suggestion"));
        assert!(prompt.contains("```blueprint-note"));
        assert!(prompt.contains("inspect_session"));

        // Must NOT inherit coding/editing/Codex-layout instructions from V2_SYSTEM_PROMPT
        assert!(!prompt.contains("EDITING AND BUILDING"));
        assert!(!prompt.contains("FINAL REPLY LAYOUT"));
        assert!(!prompt.contains("io.apply_edits"));
        assert!(!prompt.contains("process.exec_command"));
    }
}
