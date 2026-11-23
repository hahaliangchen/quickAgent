# QuickAgent 会话极速加载与编辑草稿防重复吞吐改造计划

> 目标仓库：`/home/goodtime/Documents/project/codex-workspace-mcp`（分支 `feat/context-as-code-v2`，**禁止修改 `master`**）
> 本计划汇总两项高收益核心优化：
> 1. **会话加载性能与数据库治理**：消除 5–17s 的 SQLite 写锁排队，将单会话事件载荷从 10–15 MB 降至 <300 KB，把 `WebKitWebProcess` 内存从 1 GB 拉回轻量级；
> 2. **编辑/批量编辑防重复吞吐**：利用 Context-as-Code v2 持久 JS-VM 特性自动暂存失败编辑草稿（`globalThis.__lastFailedEdit`）+ `line_hint` 行号消歧，避免单处 `old_text` 歧义/失配导致 AI 重吐数千行 `new_text`。

---

## 一、实测基线数据（2026-11-23 本机 `codex_state.db` 采样）

| 观测项 | 当前实测值 | 根因定位 |
| --- | --- | --- |
| `GET /agent/tasks/{id}/events`（并发写时） | **3.8s ~ 17.6s** | `init_db` / `open_db` / `request_context::connection` 每次连接都跑 10+ 条 DDL / `ALTER TABLE`，把 WAL 无锁读拖进全局写锁排队 |
| 同接口（空闲时 / 纯 SQLite SELECT） | 173 ms / **13 ms** | 15 MB JSON 先 `serde_json::from_str` 成 `Value` 再重新序列化 |
| 单会话事件载荷 | **10.2 ~ 15.3 MB** | `tool/result` 占 **43.45 MB**（单条最大 4.23 MB）；`workspace/changes` 占 **8.90 MB**（1,074 条全量快照）；`assistant/delta` 占 **32,292 行（68% 总行数）** |
| 数据库总大小（仅 6 个会话） | **412 MB** | `agent_request_contexts` 占 **233.55 MB**（584 行），默认对所有任务无限录全文 7 天 |
| 桌面端 `WebKitWebProcess` RSS | **1,004 MB** | 切换会话全量下载并 `JSON.parse` 15 MB 原始事件 |
| `apply_edits` 单处失配/歧义重试成本 | 整批 `new_text` 重吐（1000–3000+ token） | `batch preflight` 原子失败后无草稿保留，`TextEdit` 无行号消歧参数 |

---

## 二、专题 A：会话极速加载与 SQLite 治理

### A1（P0）：进程级按路径去重 Schema 初始化（彻底消灭读接口抢写锁）

**涉及文件**：
- `src/context/database.rs`（`init_db`）
- `src/server/agent_service/db.rs`（`open_db`）
- `src/context/request_context.rs`（`connection`）

**改造要点**：
1. 分别按 canonical `db_path` 维护进程内已初始化集合（例如 `OnceLock<Mutex<HashSet<PathBuf>>>`，并考虑文件重建/inode 变更或测试清理临时文件时自动重新初始化，确保单元测试 100% 隔离）；
2. 首次打开某个 DB 文件时执行完整的 `PRAGMA journal_mode=WAL` + `CREATE TABLE IF NOT EXISTS` + 增量列迁移（且把直接 `let _ = conn.execute("ALTER TABLE ...")` 全部改为先查 `pragma_table_info` 缺失时再 `ALTER`，避免任何无意义的写锁尝试）；
3. 后续打开同一 DB 文件仅执行 `Connection::open(&db_path)` + `busy_timeout(30s)` + `PRAGMA foreign_keys=ON`（轻量只读/连接级 pragma），**0 条 DDL、0 次写锁竞争**；
4. 同步检查 `stream_events`（`src/server/agent_service/routes.rs:547`）：SSE 轮询循环只读查询不再触发任何写锁。

### A2（P1）：事件流瘦身、RawValue 透传与 `tool/result` 按需懒加载

**涉及文件**：
- `src/server/agent_service/routes.rs`（`get_events` + 新增单条详情接口）
- `src/server/ai_proxy.rs`（注册单条事件路由）
- `frontend/src/api.ts`、`frontend/src/components/`（折叠工具卡片展开时按需加载）

**改造要点**：
1. **已结算轮次过滤冗余事件（仅影响 `get_events` 历史加载，不破坏 `stream_events` 实时流）**：
   - 若某轮已存在 `assistant/message` 或 `turn/end`，在 `get_events` 返回时**跳过该轮之前的 `assistant/delta`**（32,292 行 → 仅保留运行中未结算轮次的 delta）；
   - 同一 `turn` 内的多条 `workspace/changes` 快照只保留**最新一条**（1,074 行 → ~25 行），完整保留历史各轮的最终变更汇总；
   - 核对前端 `chatItems` / `FlowView` / `CodexRightPanel` 的全部消费清单，**绝不误删 `FlowView` 或右侧变更面板依赖的任何事件**。
2. **`tool/result` 大结果截断预览 + 单条懒加载**：
   - 新增接口：`GET /agent/tasks/{task_id}/events/{seq}`（按主键 `(task_id, seq)` 毫秒级精准返回单条完整事件 JSON）；
   - `get_events` 中对 `kind == "tool/result"` 且 `data.len() > 16 * 1024`（16 KiB）的记录：
     - 保留完整结构外壳（`actor`、`flowNodeId`、`tool_call_id`、`name`、`is_error`、`status`、`diff` 摘要等 UI 渲染必填字段）；
     - 仅对超长文本载荷做安全 UTF-8 截断预览（前 4 KiB），并标记 `truncated: true`、`full_bytes: <len>`、`detail_seq: <seq>`；
   - 前端工具结果展开抽屉（`ToolRow` / `FlowView` 详情弹窗）：检测到 `truncated: true` 且用户实际点击展开时，调用 `GET /agent/tasks/{task_id}/events/{seq}` 拉取完整结果并本地缓存。
3. **消除全量 `serde_json::Value` 往返解析**：
   - 对无需裁剪的普通事件，使用 `serde_json::value::RawValue` 直接包裹数据库里的 JSON 字符串，避免 10+ MB 数据先反序列化再重新序列化。

### A3（P2）：`agent_request_contexts` 体积封顶治理

**涉及文件**：
- `src/context/request_context.rs`

**改造要点**：
1. 保持每步 `metadata`（耗时、token、命中率、actor、turn/step）永久可读，对 `request_json` 全文增加默认保留上限：
   - 每个 `task_id` 默认保留最近 **20 条**非 pending 记录的完整 `request_json`（或总 `request_json` 字节数上限 **16 MiB/任务**），超出后将更早记录的 `request_json` 置为 `'null'` 并标记 `$.body_expired = true`（复用现有的 `expire_bodies` 语义）；
   - 当用户显式开启该任务的上下文调试开关时可放宽保留配额（如最近 60 条 / 64 MiB）。

---

## 三、专题 B：编辑与批量编辑防重复吞吐优化

### B1：JS-VM 失败草稿自动暂存（`globalThis.__lastFailedEdit`）

**涉及文件**：
- `src/runtime/agent_program/`（给 `Runtime` 增加 `set_global_json(&mut self, name: &str, value: Value)`，直接写入 `globalThis` 属性并随堆快照持久化）
- `src/runtime/single_agent/vm_driver.rs`（`ActionKind::Tool` 分支）
- 系统 Prompt（`src/runtime/single_agent/` 对应的提示词片段）

**改造要点**：
1. 在 `drive_vm_segment` 执行工具调用（`ActionKind::Tool { name, args }`）前，对编辑/写入类工具（`apply_edits`、`edit_file`、`write_file`）保留一份轻量参数引用（单次入参 `<= 2 MiB` 才暂存，避免污染 VM 堆）；
2. 当返回 `is_error == true`（或 `apply_edits` 在 `atomic: false` 下存在部分失败文件）时：
   - 构造结构化草稿对象写入 `globalThis.__lastFailedEdit`：
     ```js
     {
       tool: "apply_edits",          // 或 "edit_file" / "write_file"
       step: 14,
       args: { ... },                // 完整原始入参（含所有已生成的 new_text！）
       error: "...",                 // 本次报错摘要
       failed_files: [ ... ]         // 若为 apply_edits，标出失败项的 file_index / path / edit_index
     }
     ```
   - 同时维护环形数组 `globalThis.__failedEdits`（最多保留最近 3 次失败草稿，超出丢弃最旧一条，严格控内存）；
   - 在返回给模型的错误对象/消息里附带一行明确、可直接照抄的恢复提示：
     `Draft preserved in globalThis.__lastFailedEdit.args (patch only old_text/line_hint on the failed item and re-call apply_edits(globalThis.__lastFailedEdit.args) without regenerating new_text).`
3. 在 Agent 系统 Prompt 中增加 2 行简明指引：
   - 当 `apply_edits` / `edit_file` 因 `old_text` 歧义或未命中失败时，优先读取 `globalThis.__lastFailedEdit.args` 仅修正失败项的 `old_text` 或补充 `line_hint` 后重提交，禁止整段重新输出未变更的 `new_text`。

### B2：`TextEdit` 行号消歧（`line_hint`）与结构化定位信息

**涉及文件**：
- `src/tools/workspace.rs`（`TextEdit` 结构体与 JSON Schema 定义）
- `src/tools/file_edit.rs`（`text_edits` 匹配逻辑）
- `src/tools/batch_edits.rs`（`apply` 失败明细与 `atomic` 行为）

**改造要点**：
1. **给 `TextEdit` 增加可选字段 `line_hint: Option<usize>`（1-based 行号）**：
   - `#[serde(default)] pub line_hint: Option<usize>`，完全向后兼容现有调用；
   - 当 `old_text` 在文件中精确出现 **1 次**时：与现有一致直接应用；
   - 当 `old_text` 精确出现 **≥ 2 次**（如第 185 行和第 283 行）：
     - 若未提供 `line_hint`：保持现有安全拒绝，报错列出所有命中行号，并提示可补充更多上下文**或传入 `line_hint`**；
     - 若提供了 `line_hint`：计算所有精确命中起始行号与 `line_hint` 的距离，**仅当存在唯一最近命中且距离 ≤ 30 行（且次近命中至少比它远 10 行以上）时自动消歧应用**；否则仍拒绝执行，绝不猜错！
2. **`apply_edits` 失败回报结构化定位**：
   - 在 `batch_edits.rs` preflight 报错信息中明确标出失败文件序号 `file_index`（0-based）、`path` 与 `edit_index`，方便 AI 一行代码精准定位 `__lastFailedEdit.args.files[file_index].edits[edit_index]`；
   - 保持 `atomic: true` 默认语义不变，同时在 `apply_edits` schema 描述中明确说明修改多个互不依赖文件时可传 `atomic: false` 启用按文件原子提交。

---

## 四、验收标准

### 专题 A（性能与库治理）验收
1. **并发读写压测测试（Rust）**：后台持续向 `agent_task_events` 写数据时，并发发起 50 次 `open_db` + `get_events`，验证读请求 **0 次触发 DDL 写锁**，P95 延迟在毫秒级。
2. **事件瘦身与懒加载测试（Rust + 前端）**：
   - 已结算轮次 `assistant/delta` 被过滤、同轮 `workspace/changes` 仅留最新、大 `tool/result` 返回截断预览且 `GET /agent/tasks/{id}/events/{seq}` 可按需拉取完整原文；
   - 运行中未结算轮次 delta 完整保留，`FlowView` 与 `CodexRightPanel` 变更统计正常。
3. **`agent_request_contexts` 体积限制测试（Rust）**：连续写入 40 步大请求体后，旧步骤 `request_json` 自动回收为 `'null'` 且 `metadata` 完好。
4. **真实桌面库对比验收**：重启 `grow-desktop` 后在并发任务期间实测 `GET /agent/tasks/{id}/events`，验证载荷与耗时大幅下降。

### 专题 B（编辑防重复吞吐）验收
1. **`line_hint` 单元测试（`file_edit.rs`）**：多处精确命中时，无 `line_hint` 拒改、带准确 `line_hint` 唯一消歧命中、带模糊/超窗 `line_hint` 仍安全拒绝。
2. **JS-VM `__lastFailedEdit` 跨段测试（`vm_driver` / `agent_program`）**：第 1 段 JS `apply_edits` 因歧义失败后，第 2 段 JS 直接改 `globalThis.__lastFailedEdit.args.files[1].edits[0]` 重调一次成功，无需重吐 `new_text`。
3. **全量回归**：编译 0 警告，现有 `file_edit`、`batch_edits`、`agent_service`、`agent_program` 测试与前端 `typecheck`/单元测试全绿。

---

## 五、执行约束
- 保持在 `feat/context-as-code-v2` 分支，**禁止修改 `master`**；
- 编译前用 `df -h .` 确认可用空间 > 20 GB；
- **不要终止正在运行的 `grow-desktop` 进程**，完成代码与测试后统一由宿主构建同步与重启实测。
