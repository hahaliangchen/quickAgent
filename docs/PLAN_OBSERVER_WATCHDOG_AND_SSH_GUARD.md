# 蓝图观察者精准化、卡死看门狗与 SSH 静默超时治理计划

> 背景：2026-11-26 `rag-go` 真实长任务压测中，刚上线的可拆卸蓝图观察者成功抓出了卡死的第 26 步，但同时暴露出 4 个高价值工程问题：
> 1. **旧红线误判越界**：`blueprint.inspect_session` 只返回第 1 轮 `agent_tasks.prompt`，没返回后续轮次的 `user/message` 与 `user/interjection`，导致观察者把第 1 轮「不要改代码、不重启服务」当成永久红线，误报第 3 轮用户明确授权的「修复 `anydoc_image_vlm.go` 并仅重启 9005」为越界。
> 2. **看不到运行中工具的实时流式终端输出**：`ssh__exec` / 本地 shell 在结束前不会落盘 `tool/result`，观察者只能看到已落盘的历史事件，看不到实时终端尾部刚打出的 `422: selected document does not exist`，只能猜测进程是否还在后台。
> 3. **同步 `ssh__exec` 死等 2 小时**：模型手写 `for i in 1..360; sleep 20` + `pgrep -f` 匹配到自身 bash 命令行，导致脚本 17:45:29 报错退出后，命令仍静默空转死等。
> 4. **纯事件触发观察者在「静默卡死」时不会自动唤起**：主会话卡在同一个未完成 `tool/call` 里 0 新事件时，需要一个零 token 消耗的本地看门狗在卡住超阈值后**单次**唤起观察者（绝不死循环）。

---

## 设计原则

1. **保持观察者 100% 可拆卸**：前端新增逻辑全部留在 `frontend/src/experimental/blueprintObserver/` 内，不污染主会话渲染与核心 Agent 状态机。
2. **绝不死循环、绝不硬编码魔法步数**：观察者严格采用「语义事件触发 + 单次静默卡死唤醒」，同一未完成 `tool/call` 最多唤醒 1 次看门狗。
3. **工具层治本优先于提示词叮嘱**：`ssh__exec` 静默无输出超时由 Rust 宿主强制兜底，模型无法绕过；提示词铁律作为第二道防线减少低级 bash 坑。
4. **最新用户指令覆盖历史约束**：观察者永远以按时间排序的 `user_instructions` 时间线（尤其是最后一轮 `user/message` / `user/interjection`）为准，避免把旧轮红线当成永久禁令。

---

## 阶段一：修复观察者双盲区（`blueprint.inspect_session` 升级）

### 1.1 返回完整用户指令时间线（消除「第 1 轮旧红线误判」）
- **修改文件**：
  - `src/server/builtin_plugins/blueprint/session_dossier.rs`
  - `src/server/builtin_plugins/blueprint/tool.json`
  - `src/server/builtin_plugins/blueprint/tests.rs`
- **实现细节**：
  - 遍历 `agent_task_events` 时，收集所有 `user/message` 与 `user/interjection` 事件，输出新字段：
    ```json
    "user_instructions": [
      { "turn": 1, "seq": 1, "kind": "user/message", "text": "..." },
      { "turn": 3, "seq": 210, "kind": "user/message", "text": "..." },
      { "turn": 3, "seq": 245, "kind": "user/interjection", "text": "..." }
    ],
    "latest_user_instruction": {
      "turn": 3,
      "seq": 245,
      "kind": "user/interjection",
      "text": "..."
    }
    ```
  - 保留 `task.prompt` 兼容旧消费者，但将单条 `user_instructions` 合理裁剪（如旧轮每条 ≤ 800 字，最新 2 轮每条 ≤ 2500 字，总数上限 20 条保留最新），确保新授权指令完整不截断。
  - 在返回结果或提示词中明确语义：**当后续轮次的 `user_instructions` 与第 1 轮 `task.prompt` 存在冲突（如放开改代码、允许重启指定服务）时，以最新轮次用户授权为准**。

### 1.2 接入运行中工具（SSH / Shell）的实时流式终端尾部（`in_flight_tools`）
- **修改文件**：
  - `src/server/builtin_plugins/blueprint/session_dossier.rs`
  - `src/server/builtin_plugins/ssh/live.rs`（复用已有的实时终端环形缓冲，新增按 `task_id` 读取最新尾部 30 行的只读 helper）
  - `src/server/builtin_plugins/blueprint/tests.rs`
- **实现细节**：
  - 在 `inspect_session` 扫描事件流时，维护 `in_flight_calls: BTreeMap<String, InFlightCall>`：
    - 遇到 `tool/call` 入表（记录 `call_id`, `seq`, `step`, `tool`, `target`, `started_at_ms`）；
    - 遇到匹配的 `tool/result` 出表；
    - 遇到 `shell/output`（本地命令流式输出）时，保留最后 30 行（按 `call_id` 或最近活跃调用归档）并更新 `last_output_at_ms`。
  - 遍历结束后，对于仍然在 `in_flight_calls` 中的未完成工具调用：
    - 若工具为 `ssh__*`，顺手向 `crate::server::builtin_plugins::ssh::live` 查询该 `task_id` 对应实时通道的最后 30 行脱敏输出与最后输出时间戳 `last_output_at_ms`；
    - 计算 `elapsed_sec`（距开始秒数）与 `idle_sec`（距最后一行输出秒数）；
    - 输出到顶层字段：
      ```json
      "in_flight_tools": [
        {
          "call_id": "call_26",
          "seq": 1412,
          "step": 26,
          "tool": "ssh__exec",
          "target": "server:srv-201",
          "elapsed_sec": 312,
          "idle_sec": 295,
          "tail_lines": [
            "HTTP 422: selected document does not exist",
            "..."
          ]
        }
      ]
      ```
  - **效果**：观察者一次调用 `blueprint.inspect_session` 就能直接看到「第 26 步已跑 5 分钟、近 4 分钟零新输出、最后一行报错是 `422: selected document does not exist`」，直接报出根因而不再瞎猜进程死活。

### 1.3 更新观察者提示词规则
- **修改文件**：
  - `frontend/src/experimental/blueprintObserver/observerProtocol.ts`
  - `frontend/tests/blueprint-observer.test.mjs`
- **实现细节**：
  - 在 `buildObserverInspectPrompt` 和 `buildObserverUserChatPrompt` 中明确强调两条判据：
    1. **最新授权优先**：判定「是否越界/跑偏」必须对照 `user_instructions` 全量时间线与 `latest_user_instruction`，禁止拿第 1 轮已被后续轮次覆盖的旧约束误报越界；
    2. **先看运行中终端尾巴**：优先检查 `in_flight_tools` 的 `elapsed_sec`、`idle_sec` 与 `tail_lines`；若已明确报错或长时间零新输出，直接建议 `action: "stop"` 或 `stop_and_continue` 并引用终端报错原文。

---

## 阶段二：工具层治本（`ssh__exec` 静默无输出超时 + 轮询铁律）

### 2.1 `ssh__exec` 新增静默无输出超时（`idle_timeout_secs`）
- **修改文件**：
  - `src/server/builtin_plugins/ssh/client.rs`（或执行流式读取循环处）
  - `src/server/builtin_plugins/ssh/package.rs`
  - `src/server/builtin_plugins/ssh/tools.json`
  - `src/server/builtin_plugins/ssh/integration_tests.rs`
- **实现细节**：
  - 在 `ssh__exec` 参数中新增可选字段：
    - `idle_timeout_secs`：`integer`，默认 **`120`**（秒），范围 `0..=1800`；传 `0` 显式关闭静默超时（仅限明确知道长时间静默计算的场景，如大模型离线转换）。
  - 执行远端命令读取 stdout/stderr 时，除了现有的总超时 `timeout_secs`，维护一个 `tokio::time::sleep(Duration::from_secs(idle_timeout_secs))` 计时器：
    - 每收到任意非空 stdout/stderr 字节，重置静默计时器；
    - 一旦连续 `idle_timeout_secs` 秒 **0 字节新输出**：
      1. 主动关闭远端执行通道（发送信号/EOF/关闭 channel）；
      2. 正常返回结构化结果（而非硬挂 30 分钟）：
         ```json
         {
           "status": "idle_timeout",
           "timed_out": true,
           "idle_timeout_secs": 120,
           "exit_code": null,
           "stdout": "...（保留最后尾部输出）",
           "stderr": "...",
           "hint": "远端命令连续 120 秒无任何新输出已被自动中断，请检查进程是否已退出、卡死或等待交互输入。"
         }
         ```
  - **测试覆盖**：在 `ssh/integration_tests.rs` 新增集成测试，执行 `echo start; sleep 10` 并传 `idle_timeout_secs: 1`，验证 ~1 秒内即自动返回 `status == "idle_timeout"` 且 `stdout` 保留 `"start\n"`。

### 2.2 在 SSH 工具说明与 `SKILL.md` 固化「轮询铁律」
- **修改文件**：
  - `src/server/builtin_plugins/ssh/SKILL.md`
  - `src/server/builtin_plugins/ssh/tools.json`（`ssh__exec` 描述）
- **固化三条铁律**：
  1. **禁止长阻塞死循环**：单次 `ssh__exec` 轮询等待不超过 60–90 秒，禁止手写几十上百次 `sleep` 循环；长任务必须后台启动（重定向日志 + 落盘退出码 `.rc`）并分步短查。
  2. **禁止 `pgrep -f <命令片段>` 判活**：极易匹配到当前 shell 命令行自身造成死等；必须捕获真实 `$!` PID 并落盘（如 `nohup cmd > run.log 2>&1 & echo $! > run.pid`），或用 `(cmd; echo $? > run.rc) > run.log 2>&1 & echo $! > run.pid`，判活只用 `kill -0 "$(cat run.pid)"` + 检查 `run.rc`。
  3. **连续无变化即止损**：连续 2 次检查日志行数与关键业务指标零变化，或上游已达终态但下游任务数仍为 0 时，立即停止等待并转入服务日志与源码排查。

---

## 阶段三：观察者「静默卡死看门狗」单次唤醒（零消耗）

### 3.1 前端零消耗卡死看门狗（单次触发，绝不循环）
- **修改文件**：
  - `frontend/src/experimental/blueprintObserver/observerProtocol.ts`
  - `frontend/src/experimental/blueprintObserver/BlueprintObserverHost.tsx`
  - `frontend/tests/blueprint-observer.test.mjs`
- **实现细节**：
  - 在 `observerProtocol.ts` 新增纯函数 `findStalledInFlightTool(events, nowMs, thresholdMs = 180_000)`：
    - 找出当前尚未收到对应 `tool/result` 的最新 `tool/call`（及其 `callId`、`seq`、`name`、`startedAt`）；
    - 同时查看该 `tool/call` 之后最新一条任何事件的时间戳 `lastActivityAt`；
    - 仅当 `nowMs - lastActivityAt >= thresholdMs`（默认 3 分钟完全静默）时，返回 `{ callId, seq, toolName, idleSec }`。
  - 在 `BlueprintObserverHost.tsx` 中，**仅当 `prefs.enabled && prefs.autoInspect && watchedRunning` 时**挂一个轻量的 15 秒本地检查定时器（不发任何网络请求、不调任何模型）：
    - 维护 `watchdogFiredCallIdsRef: Set<string>`；
    - 命中 `findStalledInFlightTool` 且该 `callId` **从未唤醒过**时，加入 `Set` 并**单次**调用 `triggerInspect({ kind: 'stalled_tool', label: '疑似卡死看门狗（单次唤醒）', detail: ... })`；
    - 同一个 `callId` 绝不重复唤起第二次，彻底杜绝任何死循环可能。

---

## 阶段四：验收与发布清单

### 自动化测试
1. **Rust 单元测试 & 集成测试**：
   - `cargo test --bin codex-workspace-mcp --features internal-daily-report -- builtin_plugins::blueprint::`
     - 验证 `user_instructions` 多轮时间线与 `latest_user_instruction`
     - 验证 `in_flight_tools` 能捕获未完成 `tool/call` 的 `elapsed_sec`、`idle_sec` 与 `tail_lines`
   - `cargo test --bin codex-workspace-mcp --features internal-daily-report -- builtin_plugins::ssh::`
     - 验证 `ssh__exec` `idle_timeout_secs` 静默超时自动中止并保留已输出尾部
2. **前端类型检查、单测与构建**：
   - `npm --prefix frontend run typecheck`
   - `node --test frontend/tests/blueprint-observer.test.mjs`（覆盖 `findStalledInFlightTool`、单次唤醒防重入、新提示词约束断言）
   - `npm --prefix frontend run build`

### 桌面端发布与手工验收
1. 检查磁盘空间（> 20 GB）与 SQLite `agent_tasks` 无 `running`/`cancelling` 任务。
2. `cargo build --release --bin codex-workspace-mcp --features internal-daily-report`
3. 同步产物到 `desktop/target/debug/grow-agent` 与 `desktop/binaries/grow-agent-x86_64-unknown-linux-gnu` 并重启 `grow-desktop`。
4. **手工场景验收**：
   - 场景 A（多轮授权覆盖）：第 1 轮说「不要改代码」，第 2 轮说「允许改 `foo.ts`」，验证观察者不再误报「偏离第 1 轮红线」。
   - 场景 B（实时终端尾巴 + 静默超时）：让远端执行 `echo "422 error"; sleep 300`，观察 120s 内 `ssh__exec` 自动 `idle_timeout` 返回尾部报错；或在关闭 `idle_timeout_secs`（设为 `0`）时，观察 3 分钟后看门狗单次唤起观察者并直接报出 `"422 error"` 弹出「确认停止运行」卡片。
