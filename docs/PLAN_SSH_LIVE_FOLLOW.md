# SSH 工作台「AI 实时终端跟随 & 会话联动」实施方案（方案 A）

> 目标：当用户在项目会话里让 AI 连接服务器排查或部署时，桌面端自动切到「远程终端」页；AI 与人工**彻底隔离、各自使用独立终端标签**（AI 专属只读直播终端实时滚动命令与输出；人工随时开独立交互 PTY，互不抢键盘、互不串台），右侧 SFTP 目录自动跟随刷新，最右侧 AI 聊天抽屉自动展开并同步展示当前会话（含 `prod` 审批卡片）。

---

## 1. 核心架构原则

1. **人机终端完全隔离**：
   - AI 远程执行仍走独立的 SSH `exec` 通道（保证精确 `exit_code`、精确边界、不被远端提示符或交互程序卡死），同时通过内存环形缓冲 + 广播流实时镜像到前端只读 `xterm.js` 标签页。
   - 人工交互终端仍走现有的独立 WebSocket PTY（`TerminalPane.tsx`），只有人工手动点击“＋ 新建终端”或“打开人工终端”才建立；人机永不共用同一个 stdin/PTY。
2. **只读直播零风险**：
   - AI 实时流 WebSocket（`/agent/ssh/servers/{id}/ai-stream`）严格只读，不主动建立 SSH 连接、不接受浏览器发往远端的输入帧，仅订阅本进程内 `(task_id, server_id)` 的广播缓冲；同样受 `browser_only` 精确同源校验与单次票据保护。
3. **防抢焦点与终端保活**：
   - SSH 工作台改用保活挂载（类似 `officeMounted` 的 `display: none` 隐藏），切回代码对话页不会断开已打开的人工终端或 AI 实时终端。
   - 自动跳转仅对**当前正在查看且实时新产生的**远程 SSH 工具调用生效（绝不因为点开历史会话而误跳转）；并在工作台顶栏提供可随时关闭的“跟随 AI 操作”开关。

---

## 2. 后端改动（`src/server/builtin_plugins/ssh/`）

### 2.1 新增 `live.rs`：内存环形缓冲 + 实时广播中心
- 全局单例 `Hub`（`OnceLock<Hub>`），按 `(task_id, server_id)` 维护频道：
  - 容量上限：最多保留 32 个活跃频道（超限时按最后活跃时间淘汰最旧频道），单频道环形缓冲上限 **256 KiB**（按行或块裁掉最旧字节），避免长日志撑爆内存；
  - 广播通道：`tokio::sync::broadcast::Sender<LiveMessage>`（容量 256）；
  - `LiveMessage` 枚举：
    - `Bytes(Vec<u8>)`：面向 `xterm.js` 的 ANSI 文本字节（自动把裸 `\n` 规范化为 `\r\n`）；
    - `Activity { action: String, remote_path: Option<String>, detail: String }`：用于通知前端同步刷新 SFTP 与“最近活动”列表。
- 对外辅助函数：
  - `emit_exec_start(task_id, server_id, cwd, command)`：写入青色命令头，例如 `\r\n\x1b[36m[AI · <cwd>]\x1b[0m $ <command>\r\n`；
  - `emit_exec_chunk(task_id, server_id, is_stderr, chunk)`：实时推送 stdout/stderr 字节（stderr 可用暗黄/淡红区分，或保持原始 ANSI）；
  - `emit_exec_end(task_id, server_id, exit_code, elapsed_ms)`：写入退出码与耗时徽章（0 为绿色，非 0 为红色），并发送 `Activity`；
  - `emit_sftp_event(task_id, server_id, action, remote_path, detail)`：写入黄色单行运维日志（如 `[AI SFTP] upload dist/app.bin -> /releases/app.bin (10 B)`）并发送 `Activity`（携带目标目录或文件路径）。
  - `subscribe(task_id, server_id) -> (Vec<u8>, broadcast::Receiver<LiveMessage>)`：原子返回当前历史环形缓冲快照 + 实时订阅 receiver（先重放快照再转发后续实时事件，保证晚切进页面的用户也能看到刚才执行的命令和输出）。

### 2.2 `client.rs`：支持边执行边流式回调
- 扩展 `client::exec`（或新增 `exec_with_stream`）接收可选实时回调 `on_chunk: Option<&mut (dyn FnMut(bool, &[u8]) + Send)>`：
  - 在 `while let Some(msg) = channel.wait().await` 循环中，每收到 `ChannelMsg::Data` 或 `ChannelMsg::ExtendedData { ext: 1, .. }` 就**立刻**触发回调推给 `live.rs`，同时仍保留原有的 128 KiB 截断缓冲返回给 AI 工具结果。

### 2.3 `package.rs`：接入实时广播
- 当 `ctx.task_id` 为 `Some(ref task_id)` 时：
  - `ssh__exec`：连接成功后依次调用 `emit_exec_start` → 通过流式回调逐块调用 `emit_exec_chunk` → 结束时调用 `emit_exec_end`；
  - `ssh__list_dir` / `ssh__read_file` / `ssh__tail_log` / `ssh__write_file` / `ssh__upload`：操作完成后调用 `emit_sftp_event`，传入规范化后的远程目录或文件路径。
- **安全红线**：绝不向广播流写入任何 SSH 密码、私钥口令或凭据文件内容。

### 2.4 `routes.rs`：只读 AI 实时流票据与 WebSocket 端点
- 新增两个路由（挂载在现有 `browser_only` 中间件下）：
  - `POST /agent/ssh/servers/{id}/ai-stream-ticket`：请求体 `{ "task_id": "..." }`，校验服务器存在且 `task_id` 非空，签发 30 秒有效、单次消费的只读票据（不触发 `prod` 人工确认拦截，因为该端点不连接远端也不执行任何操作）；
  - `GET /agent/ssh/servers/{id}/ai-stream?ticket=...`：校验并消费单次票据后升级 WebSocket：
    - 发送 `{"type":"ready"}`；
    - 若历史缓冲非空，立即发送 `Message::Binary(replay_bytes)`；
    - 循环转发 `LiveMessage::Bytes`（`Message::Binary`）与 `LiveMessage::Activity`（`Message::Text` JSON `{"type":"activity",...}`），附 20 秒 `Ping` 心跳；
    - 忽略浏览器发来的任何输入帧（仅响应 `Close`）。
- 更新 `browser_only` 中间件中的 WebSocket 升级白名单：允许以 `/terminal` **或** `/ai-stream` 结尾且携带精确同源 `Origin` 的 WebSocket 握手请求。

---

## 3. 前端改动（`frontend/src/`）

### 3.1 `App.tsx`：SSH 保活挂载 + 实时自动跳转 + 会话联动
1. **保活挂载**：新增 `sshMounted` 状态（首次进入或自动跳转时置 `true`），使用 `display: ssh ? 'flex' : 'none'` 渲染 SSH 视图容器（与 `officeMounted` 保持一致），避免切回主界面时断开人工终端与 AI 实时终端。
2. **监听当前会话的远程 SSH 工具调用**：
   - 监听当前活跃会话 `workspace.session.events`（且仅在 `workspace.running` 或事件 `seq` 大于当前会话初次加载时的基线时触发，**防止打开旧会话历史记录时误跳页**）。
   - 识别 6 个远程操作工具调用事件（`ssh__exec`、`ssh__list_dir`、`ssh__read_file`、`ssh__write_file`、`ssh__upload`、`ssh__tail_log`），解析 `args.server_id` 及 `args.cwd || args.path || args.remote_path`。
   - 对已处理过的 `call_id` 去重；若 `followAi` 开关为开启状态：
     - 同一轮次首次触发时自动切到 SSH 页（`setSsh(true); setSshMounted(true); setOffice(false); setGit(false); setWorkbench(false)`），并递增 `sshChatOpenSignal` 自动展开最右侧聊天抽屉；
     - 每次远程 SSH 调用均更新 `aiFocus = { seq, taskId, serverId, tool, remotePath }` 并通过 `SshContext` 传入工作台。
   - 若用户在当前轮次手动切离 SSH 页，不强行反复拉回（仅继续静默更新 `aiFocus`，等下一轮新任务或用户手动点回时仍能看到最新内容）。

### 3.2 `CodeChatDock.tsx`：支持同步绑定当前主会话
- 新增两个可选 props（不影响 Git 页原有默认行为）：
  - `linkedTaskId?: string | undefined`：当传入时，右侧抽屉自动同步显示该主会话的时间线、运行态与**审批卡片**（包括 `prod` 服务器触发的单次人工审批），并允许用户直接在同一抽屉里继续追问/插话；
  - `openSignal?: number | undefined`：递增时自动展开抽屉（`setOpen(true)`）。
- 抽屉头部保留「在主界面打开此会话」与「收起」按钮，同时显示当前绑定会话标题，让用户一眼确认右侧聊天与左侧 AI 终端属于同一会话。

### 3.3 `frontend/src/builtin_plugins/ssh/`：AI 专属只读终端标签 + SFTP 自动联动
1. **`api.ts`**：
   - 扩展 `WorkbenchContext`：加入 `aiFocus`、`followAi`、`onToggleFollowAi`；
   - 新增 `ssh.aiStreamTicket(serverId, taskId, signal)` 与 `ssh.aiStream(serverId, ticket)`。
2. **新增 `AiTerminalPane.tsx`（只读 AI 实时终端组件）**：
   - 使用 `@xterm/xterm` + `FitAddon` + `WebLinksAddon`，显式配置 `disableStdin: true`、`cursorBlink: false`；
   - 顶栏展示清晰徽标 **「AI 实时终端 · 只读」**、连接状态、**「＋ 打开人工终端」**按钮（点击在同一服务器开启独立交互式 `TerminalPane`），以及「复制选区」「加入问答草稿」；
   - 连接 `ai-stream` WebSocket：接收 `ArrayBuffer` 写入 `xterm.js`，接收 `{"type":"activity",...}` 回调父组件刷新 SFTP 与“最近活动”列表。
3. **`page.tsx`**：
   - 将标签模型扩展为判别联合：`type Tab = { kind: 'human'; id: number; profile: Server; access: Access } | { kind: 'ai'; id: string; taskId: string; profile: Server }`；
   - 顶栏增加「跟随 AI 操作」开关（默认开启），并修正 `activity` 映射加入 `exec: '执行命令'`；
   - 响应 `context.aiFocus`：
     - 若本地 `servers` 尚未包含该 `serverId`，重新拉取一次 `ssh.list()`；
     - 选中目标 `serverId`；
     - 打开或聚焦对应的 `kind: 'ai'` 标签（按 `ai:${taskId}:${serverId}` 去重复用，不重复弹出一堆标签）；
     - 若 `aiFocus.remotePath` 存在，计算目录路径（`ssh__list_dir` / `ssh__exec` 直接取路径，文件读写/上传取父目录）并下发给 `SftpExplorer`。
4. **`SftpExplorer.tsx`**：
   - 新增可选 `followTarget?: { path: string; seq: number }` prop：当其变化时，自动切换目录并刷新列表（同时不影响用户手动浏览、上传、预览）。

---

## 4. 验收与测试要求

1. **Rust 自动化测试**（在 `src/server/builtin_plugins/ssh/integration_tests.rs` 新增）：
   - `ai_stream_replays_history_streams_live_exec_and_enforces_ticket`：
     - 校验无同源头/无票据访问 `/ai-stream` 被拒绝（403/401），单次票据不可复用；
     - 连上 `/ai-stream` 后执行 `ssh__exec` 与 `ssh__upload`，验证 WebSocket 实时收到命令回显、stdout 输出、退出码以及 `activity` JSON 事件；
     - 结束后再新建第二个 `/ai-stream` 订阅者，验证能完整收到环形缓冲历史回放；
     - 校验 `prod` 服务器未审批仍被拦、且 `/ai-stream` 不泄露凭据。
   - 运行 `cargo test --bin codex-workspace-mcp -- builtin_plugins::ssh tool_wiring_invariants`，要求 **0 失败、0 警告**。
2. **前端类型检查、构建与 Playwright 测试**：
   - `npm --prefix frontend run typecheck` 0 错误；
   - `npm --prefix frontend run build` 通过；
   - 更新/补充 `frontend/tests/ssh-workbench.spec.mjs`，验证 AI 实时终端标签与人工交互终端标签并存互不干扰、SFTP 目录跟随与 `CodeChatDock` 同步显示当前会话，并运行 `npm --prefix frontend run test:ssh` 通过。
3. **执行约束**：
   - 严格保持分支 `feat/context-as-code-v2`，**禁止碰 `master`**；
   - 编译前用 `df -h .` 检查磁盘可用空间 > 20 GB；
   - **不要重启正在运行的 `grow-desktop` 进程**（完成后由宿主统一检查、编 release、同步桌面端二进制并重启）。
