# 微信式「查找聊天记录与素材」+ 跨会话检索增强施工计划

> **适用仓库与分支**：`codex-workspace-mcp` 当前分支 `feat/context-as-code-v2`（**严禁触碰 `master`**）。
>
> **验证铁律**：需要起本地服务做浏览器端到端验收时，**必须先阅读 `.agents/skills/self-verify/SKILL.md`，使用 `./scripts/start-isolated-agent.sh`（`--isolated` 模式）启动完全隔离的验证实例**，严禁直接跑 `./target/.../codex-workspace-mcp --desktop-agent` 触碰宿主桌面端主库！

---

## 0. 目标与范围

让 QuickAgent 的会话具备类似微信「查找聊天内容」的体验，同时把同等能力开放给 Agent 工具层，实现**人机共用一套会话记录与素材索引**：

1. **前端：微信式「查找会话记录与素材」弹窗（`ChatHistoryModal`）**
   - 对话页顶栏增加明确入口按钮（放大镜/聊天记录图标，支持 `Ctrl+F` / `Cmd+F` 在主对话打开、按 `Esc` 关闭）；
   - 顶部搜索框 + 范围切换：**「当前会话」 / 「全部会话」**；
   - 4 个清晰分类 Tab（不放无关的音乐/视频号）：
     1. **全部**：按时间倒序展示聊天消息（用户提问 + AI 回复），显示发送角色、时间戳/日期、关键词高亮（跨会话时显示所属会话标题胶囊）；点击某条消息关闭弹窗并**自动滚动定位并短暂高亮该消息**（跨会话时先切过去再定位）；
     2. **图片**：统一聚合本会话（或全部会话）的**用户上传图片**（`agent_attachments` 中 `media_type LIKE 'image/%'` 或 `image_artifact` 非空）+ **AI 视觉/浏览器截图与生成图**（`agent_visual_artifacts`），按日期分组渲染缩略图网格（复用现有 `/agent/tasks/{task_id}/attachments/{receipt}` 与 `/agent/tasks/{task_id}/visual-artifacts/{id}/image`），支持点击大图灯箱预览 + 按钮「定位到原消息」；
     3. **文件**：统一聚合**用户上传的非图片附件**（`agent_attachments`）+ **AI 改动/新建的工作区文件**（`agent_file_changes`，含新增/删除行数统计与轮次），支持直接触发右侧文件预览（`onFilePreview`）或附件下载，也可点「定位到原消息」；
     4. **日期**：月历视图，自动标亮「有聊天记录的日期」与条数点标，支持切换上下月；点击有记录的某一天，在下方/右侧立刻列出该日消息，点击任意一条直跳原消息。
2. **后端：统一会话历史与素材查询 HTTP 接口**
   - 在 `src/context/session_browser.rs`（挂载在 `src/server/ai_proxy.rs` 现有 `/agent/sessions/...` 路由旁）新增轻量只读接口，直接查询现有 `codex_state.db`（不破坏任何既有表结构与迁移）：
     - `GET /agent/history/messages?task_id=...&q=...&date=YYYY-MM-DD&from=...&to=...&limit=50&offset=0`
       - `task_id` 为空时搜全部会话，非空时限定该会话；返回 `{ total, items: [{ task_id, task_title, seq, turn, role, timestamp, snippet, text }] }`；
     - `GET /agent/history/media?task_id=...&kind=images|files&q=...&date=YYYY-MM-DD&limit=80&offset=0`
       - `kind=images`：合并 `agent_attachments` 图片项与 `agent_visual_artifacts` 视觉产物（附 `url`、`thumb_url`、`label`、`source: 'upload' | 'artifact'`、`timestamp`、`turn`、`seq`、`task_id`、`task_title`）；
       - `kind=files`：合并 `agent_attachments` 非图片附件与 `agent_file_changes` 改动文件（附 `name`、`path`、`source: 'upload' | 'workspace_change'`、`added`、`deleted`、`download_url`、`timestamp`、`turn`、`seq`、`task_id`、`task_title`）；
     - `GET /agent/history/dates?task_id=...&month=YYYY-MM`
       - 返回该月（或全部有记录的月份范围）每天的消息计数：`{ month, days: [{ date: "YYYY-MM-DD", count: 12 }], months: ["2026-11", ...] }`，供日历高亮。
   - **安全约束**：所有接口只返回当前实例状态库里的记录；附件下载与图片路由沿用既有 `task_id` 作用域校验，不暴露越权绝对路径或凭据文件。
3. **Agent 工具层补齐跨会话目录与素材查询**
   - 在 `session` 宿主库新增 `session.list_sessions`（工具名 `list_sessions`）：
     - 参数：`{ query?: string; workspace?: string; date?: string; since?: string; until?: string; limit?: number }`（全部可选，默认按 `updated_at DESC` 返回最近 20 条，最大 50 条）；
     - 返回：`{ sessions: [{ task_id, title, workspace, status, kind, created_at, updated_at, message_count, image_count, file_count }], total }`；
     - 同步注册到 `src/tools/tool_registry.rs`（`session.list_sessions`）、`src/runtime/agent_program/declarations.rs`、以及 `src/runtime/single_agent/host_tools.rs`（只读工具，无需审批）；
   - 同时顺手修复 `src/server/builtin_plugins/daily_report/activity.rs`（约 L120）：把 SQL 已经查出的 `task_id` 放进每个 `sessions` 条目中，让 AI 写日报时能顺着 `task_id` 调 `session.read_session_history` 精读对话细节。

---

## 1. 关键代码锚点（已核验）

| 模块 | 现有文件 | 复用点 |
|---|---|---|
| 后端路由挂载 | `src/server/ai_proxy.rs`（L1475-L1580） | 现有 `/agent/sessions/search`、`/agent/tasks/{task_id}/visual-artifacts/{id}/image`、`/agent/tasks/{task_id}/attachments/{receipt}` |
| 会话查询实现 | `src/context/session_browser.rs` | 现有 `search_sessions` 与 SQLite 表 `agent_tasks` / `agent_session_meta` / `agent_task_events` / `agent_attachments` / `agent_visual_artifacts` / `agent_file_changes` |
| Agent 宿主工具 | `src/tools/tool_registry.rs`（L210）、`src/runtime/single_agent/host_tools.rs`（L35, L202）、`src/runtime/agent_program/declarations.rs`（L403） | 仿照 `session.read_session_history` 注册 `session.list_sessions` |
| 日报活动聚合 | `src/server/builtin_plugins/daily_report/activity.rs`（L51-L125） | 补上 `"task_id": row["task_id"]` |
| 前端 API 封装 | `frontend/src/api.ts` | 新增 `historyMessages` / `historyMedia` / `historyDates` |
| 主对话页与定位 | `frontend/src/components/AgentPage.tsx`（L107 `scrollToMessage`、L143 `pendingJump`）、`frontend/src/App.tsx` | 复用已有 `scrollToMessage` 与 `onFilePreview`；跨会话跳转时复用 `workspace.openTask(id)` + 待跳 `pendingJump` |
| 视觉风格 | `frontend/src/components/WorkspaceModal.module.css` / `ScrollToBottom.module.css` | 遵循现有亮/暗双主题 CSS 变量（`data-ds-dark-theme`），弹窗干净克制、信息层级清晰 |

---

## 2. UI 与交互细节要求

1. **视觉与布局（参考微信「查找聊天内容」，融入 QuickAgent 风格）**：
   - 居中浮窗（宽度约 `680px`，最大高 `78vh`，小屏自适应），带柔和背景遮罩，点击遮罩或按 `Esc` 关闭；
   - 顶栏：弹窗标题（当前会话名或「全部会话记录」）+ 范围胶囊切换（`当前会话` / `全部会话`）+ 关闭按钮；
   - 搜索框：圆角输入框，防抖 200ms 触发查询，带清空按钮；
   - 分类条：`全部`、`图片`、`文件`、`日期` 4 个简洁 Tab（高亮下划线或胶囊，带数量或清晰选中态）；
   - **图片 Tab**：4 列响应式正方形缩略图网格，按日期（如 `今天` / `昨天` / `2026-11-24`）分组；悬停显示来源标签（`上传` / `截图`）与「定位」小按钮；点击缩略图打开大图灯箱（支持左右切换、查看尺寸/时间、定位到原消息）；
   - **文件 Tab**：按日期分组的清晰文件行（文件名、相对路径或大小、来源标记：`上传附件` / `AI 修改 +12 -3`、时间），点击工作区文件直接打开右侧预览面板，点击上传附件可预览/下载，右侧带「定位」按钮；
   - **日期 Tab**：紧凑月历（周一～周日），有消息的日期带柔和底色圆点；选中某天直接列出当天的聊天消息（复用「全部」消息行组件，点击即跳）；
   - **跳转体验**：点击定位到某条消息时，弹窗关闭，主对话平滑滚动到该 `seq` / `turn` 对应的消息气泡并闪一下淡蓝色高亮边框（约 1.2s 自动淡出）。如果 `ChatView` 消息 DOM 还没有 `data-event-seq` 属性，顺手在渲染消息行时挂上 `data-event-seq` 与 `data-turn`，让定位精确到单条消息（而不是只滚到轮次大致位置）。
2. **暗色与亮色模式都必须清晰美观**：不使用硬编码白底黑字，全部走现有主题变量。

---

## 3. 验收标准与闸门（全部通过后才算完成）

1. **Rust 单测（默认模式 + 内部日报模式均 0 warning）**：
   - 在 `src/context/session_browser.rs`（或对应测试模块）为新接口与 `list_sessions` 补齐单测：
     - 按 `task_id` 与全局跨会话关键词检索消息；
     - 按 `kind=images` 合并 `agent_attachments` 图片与 `agent_visual_artifacts`；
     - 按 `kind=files` 合并 `agent_attachments` 文件与 `agent_file_changes`；
     - 按 `month` 返回正确的每日计数；
     - `session.list_sessions` 按日期/项目/关键词筛选准确；
   - 跑通：
     - `cargo check`（0 warning）
     - `cargo test -- session_browser`（或新测试名）
     - `cargo test -- agent_program::declarations::`
     - `cargo test --features internal-daily-report -- daily_report`（验证 `today_activity` 包含 `task_id` 且 0 warning）
2. **前端类型与构建**：
   - `npm --prefix frontend run typecheck && npm --prefix frontend run build` 通过；
   - `npm --prefix frontend run test:ssh` 与 `npm --prefix frontend run test:blueprint` 无回归。
3. **隔离实例真实浏览器验收（必须遵守 `.agents/skills/self-verify/SKILL.md`）**：
   - 用 `./scripts/start-isolated-agent.sh` 启动独立验证实例（或带 `--isolated-dir /tmp/qa-history-verify` 预先往隔离库写入带文本、图片、文件、多日期的真实测试数据）；
   - 用 `browser.*` 打开 `GROW_READY_URL`，实测：
     - 亮色 + 暗色模式下「全部 / 图片 / 文件 / 日期」4 个 Tab 视觉截图与 `vision.analyze_images` 检查（无裁切、无重叠、对比度清晰）；
     - 点击消息/日期/图片「定位」确实滚动并高亮到对应消息；
     - 验收结束后用 `SIGTERM` / `process.kill_session` 关闭隔离实例，确认无残留进程，且绝不污染宿主库。
4. **不要提交 Git**（由用户验收后再统一提交）。
