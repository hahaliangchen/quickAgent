//! Knowing when to stop. Some failures cannot be fixed by trying harder: a
//! capability or setting the user has not configured, an operation the user,
//! the reviewer or the permission mode declined, or the same call failing the
//! same way again and again. The host marks such a result `NEEDS_USER`.
//!
//! The host found it from outside the program's own logic, so it is handled
//! like the user's interjection (design guide §7.3), not like a question: the
//! whole program pauses at its next safe step, program code cannot catch it,
//! and the blocked call is not delivered, so resuming runs it again. The turn
//! ends with what the user needs to do; the paused program is kept through
//! the next turn, whose program can continue it with `previous.resume()`.
//!
//! The main AI calling a tool directly, outside any program, gets one reply
//! to tell the user. A reply that calls tools instead ends the turn with the
//! host's own report.
use super::*;
pub(super) use crate::agent_program::runtime::NEEDS_USER;

/// How many identical failures (same tool, same arguments, same error) a turn
/// tolerates before the host treats the next one as a blocker.
const REPEAT_LIMIT: u32 = 3;

/// What makes two failures the same call failing the same way. Arguments are
/// part of it: three different pages each answering 404 are three dead ends
/// a search moves past, not one call that keeps failing.
fn repeat_key(tool: &str, arguments: &str, error: &str) -> String {
    let arguments = serde_json::from_str::<Value>(arguments).map(|value| value.to_string()).unwrap_or_else(|_| arguments.to_owned());
    format!("{tool}\u{0}{arguments}\u{0}{}", error.chars().take(500).collect::<String>())
}

/// A failure only the user can resolve, and what they need to do about it.
fn classify(error: &str) -> Option<(&'static str, &'static str)> {
    const KNOWN: &[(&str, &str, &str)] = &[
        ("WEB_NOT_CONFIGURED", "WEB_NOT_CONFIGURED", "在“设置 → 专用模型 → 联网搜索 → 搜索凭据”里选择一个已配置 Key 的 DeepSeek 提供商并保存，然后告诉我继续。"),
        ("USER_DENIED", "USER_DENIED", "你拒绝了这个操作。如果需要它，请允许后让我继续；否则告诉我换什么做法。"),
        ("AI_REVIEW_DENIED", "AI_REVIEW_DENIED", "自动审查拒绝了这个操作。请确认是否允许它（或调整权限模式），或者告诉我换什么做法。"),
        ("TOOL_PERMISSION_DENIED", "PERMISSION_DENIED", "当前权限模式不允许这个操作。请在设置里调整权限，或者告诉我换什么做法。"),
        ("permission_denied:", "PERMISSION_DENIED", "当前权限模式不允许这个操作。请在设置里调整权限，或者告诉我换什么做法。"),
        ("SANDBOX_UNAVAILABLE", "SANDBOX_UNAVAILABLE", "执行沙箱不可用（例如本机没有安装 bubblewrap）。请按错误信息处理后告诉我继续。"),
        ("GIT_UNAVAILABLE", "GIT_UNAVAILABLE", "本机没有可用的 git。请安装后告诉我继续。"),
    ];
    KNOWN.iter().find(|(marker, _, _)| error.contains(marker)).map(|(_, cause, action)| (*cause, *action))
}

fn error_text(result: &Value) -> String {
    result["error"].as_str().map(str::to_owned).or_else(|| result["message"].as_str().map(str::to_owned)).unwrap_or_else(|| result.to_string())
}

fn instruction(main: bool) -> &'static str {
    if main {
        "STOP. This needs the user; do not work around it, retry it, guess the missing data or substitute another source. Do not call any more tools: they will not run. Reply to the user in plain text now: what you were doing, the exact error, what they need to do (user_action), and what was already completed."
    } else {
        "STOP. This needs the user. The host pauses the program at this step and tells the user what to do; after they resolve it, resuming runs this step again."
    }
}

/// Mark a failed tool result that needs the user, or the third identical
/// failure this turn. Returns whether the result is now a blocker.
pub(super) fn mark(s: &Session<'_>, w: &mut Window, tool: &str, arguments: &str, result: &mut Value, is_error: bool) -> bool {
    if !is_error || result["code"] == NEEDS_USER || result["host_denied"] == true || crate::agent_program::runtime::is_resource_error(result) {
        return result["code"] == NEEDS_USER;
    }
    if w.failure_turn != s.turn {
        w.failure_turn = s.turn;
        w.failures.clear();
    }
    let error = error_text(result);
    let (cause, action) = match classify(&error) {
        Some(found) => found,
        None => {
            let count = w.failures.entry(repeat_key(tool, arguments, &error)).or_default();
            *count += 1;
            if *count < REPEAT_LIMIT {
                return false;
            }
            ("REPEATED_FAILURE", "同一个操作以同样的错误连续失败了多次，换着法子重试解决不了。请看一下这个错误，告诉我缺什么（配置、权限、文件或信息）或者该怎么处理。")
        }
    };
    let main = w.program_task.is_none();
    let original = std::mem::take(result);
    *result = json!({"code":NEEDS_USER,"name":"NeedsUser","cause":cause,"tool":tool,"error":error,
        "repeated":if cause == "REPEATED_FAILURE" { json!(REPEAT_LIMIT) } else { Value::Null },
        "user_action":action,"instruction":instruction(main),"message":format!("需要你处理：{error}"),
        "original":original});
    true
}

const BLOCKED: &str = "program/blocked";

/// Ask every frame of the running program to pause at its next step, as the
/// user's interjection does. The request holds until the main program pauses.
pub(super) async fn request_pause(s: &Session<'_>, blocker: &Value) -> Result<()> {
    append(s, BLOCKED, json!({"turn":s.turn,"blocker":blocker}), false).await?;
    Ok(())
}

/// The blocker a pause was requested for, while the main program has not
/// paused for it yet.
pub(super) async fn pause_requested(s: &Session<'_>) -> Result<Option<Value>> {
    let (task, turn) = (s.task.to_owned(), s.turn as i64);
    db(s.state.workspace.root(), move |conn| {
        Ok(conn.query_row(
            "SELECT json_extract(b.data,'$.blocker') FROM agent_task_events b WHERE b.task_id=?1 AND b.kind=?2
               AND json_extract(b.data,'$.turn')=?3
               AND NOT EXISTS (SELECT 1 FROM agent_task_events p WHERE p.task_id=b.task_id AND p.kind='program/paused' AND p.seq>b.seq)
             ORDER BY b.seq LIMIT 1",
            params![task, BLOCKED, turn],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(|raw| serde_json::from_str(&raw))
        .transpose()?)
    })
    .await
}

/// What a function's AI sees in place of the blocked result. It is read only
/// after the user resumed the program, so it says to make the call again.
pub(super) fn paused_delivery(blocker: &Value) -> Value {
    json!({"code":"PAUSED_FOR_USER","cause":blocker["cause"],"error":blocker["error"],"user_action":blocker["user_action"],
        "instruction":"The program paused at this call for the user to resolve it, and the user has since resumed it. Make this call again; if it fails the same way, report it."})
}

/// How the turn ends when a blocker paused the program.
pub(super) fn paused_lines(label: &str, stopped_at: &str, blocker: &Value) -> Vec<String> {
    let at = if stopped_at.is_empty() { String::new() } else { format!("停在 {stopped_at}，") };
    vec![
        format!("{label}{at}需要你处理：{}", blocker["error"].as_str().unwrap_or("")),
        format!("需要你做的：{}", blocker["user_action"].as_str().unwrap_or("")),
        "程序已暂停，状态都保留着。处理好后发消息让我继续：我会从这一步接着跑，已完成的步骤不重做。".to_owned(),
    ]
}

/// How to continue, for the main AI's copy of the outcome only; the user's
/// copy says what to do in their terms.
pub(super) const RESUME_HINT: &str = "When the user says it is fixed or to continue, write a new program with import * as previous from \"host:previous/promises\" and await previous.resume(): it retries the blocked step and returns the paused program's result. The paused program is kept only until the end of the next turn.";
/// Whether the main AI was already told to stop this turn.
pub(super) fn pending(s: &Session<'_>, w: &Window) -> Option<Value> {
    w.blocked.as_ref().filter(|(turn, _)| *turn == s.turn).map(|(_, blocker)| blocker.clone())
}

/// The host's report when the main AI kept calling tools after a blocker.
pub(super) fn report(blocker: &Value) -> String {
    let tool = blocker["tool"].as_str().unwrap_or("");
    let error = blocker["error"].as_str().unwrap_or("");
    let action = blocker["user_action"].as_str().unwrap_or("");
    format!("这一轮停下了：调用 {tool} 时遇到需要你处理的问题。\n错误：{error}\n需要你做的：{action}\n处理好后发消息让我继续。")
}

/// How a program that stopped on a blocker is described in its outcome.
pub(super) fn outcome_lines(label: &str, error: &Value) -> Vec<String> {
    let at = error["location"]["line"].as_u64().map(|line| format!("第 {line} 行")).unwrap_or_default();
    vec![
        format!("{label}在{at}停下，需要你处理：{}", error["error"].as_str().unwrap_or("")),
        format!("需要你做的：{}", error["user_action"].as_str().unwrap_or("")),
        "之后的步骤没有执行，处理好后发消息让我继续。".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_only_failures_are_recognized_and_others_are_not() {
        let (cause, action) = classify("web__search: WEB_NOT_CONFIGURED: choose a DeepSeek credential in Settings (设置) > 专用模型 > 联网搜索 > 搜索凭据").unwrap();
        assert_eq!(cause, "WEB_NOT_CONFIGURED");
        assert!(action.contains("设置 → 专用模型 → 联网搜索 → 搜索凭据"), "names the settings page the user actually sees: {action}");
        assert_eq!(classify("USER_DENIED: the user declined running run_program.").unwrap().0, "USER_DENIED");
        assert_eq!(classify("AI_REVIEW_DENIED: the approval reviewer declined x").unwrap().0, "AI_REVIEW_DENIED");
        assert_eq!(classify("permission_denied: write_file").unwrap().0, "PERMISSION_DENIED");
        assert!(classify("No such file or directory (os error 2)").is_none());
        assert!(classify("old_string not found in src/main.rs").is_none());
    }

    #[test]
    fn only_the_same_call_failing_the_same_way_counts_as_repeated() {
        let error = "WEB_PAGE_UNAVAILABLE: fetch returned HTTP 404; no usable page content was available";
        let page = |url: &str| repeat_key("web__fetch", &json!({"url":url,"max_chars":12000}).to_string(), error);
        assert_ne!(page("https://a.example/x"), page("https://b.example/y"), "different pages are different calls");
        assert_eq!(page("https://a.example/x"), page("https://a.example/x"));
        // The same arguments written with other spacing are still the same call.
        assert_eq!(
            repeat_key("web__fetch", r#"{"url":"https://a.example/x","max_chars":12000}"#, error),
            repeat_key("web__fetch", r#"{ "url": "https://a.example/x", "max_chars": 12000 }"#, error)
        );
        assert_ne!(repeat_key("read_file", r#"{"path":"a"}"#, "e1"), repeat_key("read_file", r#"{"path":"a"}"#, "e2"));
    }
}
