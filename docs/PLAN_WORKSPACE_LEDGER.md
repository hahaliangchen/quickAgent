# 根治 `SHELL_BUSY`：文件级 WAL 变更账本 + 取消 Shell 全局写锁实施方案

> 背景：目前只要工作区里有任何未退出的 `shell_xxx` 会话（无论是 1 秒超时转后台的短命令、后台 dev server、只读命令，还是同仓库另一会话的测试），`host_tools.rs` 的 `ensure_idle()` 与 `shell_process.rs` 长持的 `MutationGuard::Workspace` 都会一刀切拒掉所有 `write_file` / `edit_file` / `replace_range` / `batch_edits`，频繁触发 `SHELL_BUSY` 打断 Agent 脚本。
>
> 目标：借鉴 SQLite 无锁读 + `db_writer` 串行写队列的解耦思想，**将“真实文件落盘”与“变更追踪记账”彻底解耦**——文件按单文件路径锁实时落盘，变更通过文件级 WAL 账本记录，shell 结束时按账本对账补记，彻底消除日常编辑和跨会话操作中的 `SHELL_BUSY`。

---

## 1. 核心架构设计（WAL 变更账本）

### 1.1 不采用“延迟落盘草稿”，坚持“实时落盘 + WAL 账本”
- 文件修改（`write_file` / `edit_file` / `replace_range` / `batch_edits` / `delete_path` / `rename_path` / `create_dir`）**直接写真实磁盘**（保证编译器、dev server、`read_file`、Git 与前端预览永远看到单一真实文件）。
- 仅在单文件路径锁（`FILE_MUTATIONS` + `ResourceClaim`）保护下完成读写，**绝不等待全工作区级 shell 写锁**。
- 每次工具文件写入后，经由 `crate::context::db_writer::write_async` 统一串行写队列向新表 `agent_change_ledger` 追加一条不可变账本记录：
  ```sql
  CREATE TABLE IF NOT EXISTS agent_change_ledger (
      seq INTEGER PRIMARY KEY AUTOINCREMENT,
      workspace TEXT NOT NULL,
      task_id TEXT NOT NULL,
      turn INTEGER NOT NULL,
      path TEXT NOT NULL,
      source TEXT NOT NULL,          -- 'tool' | 'shell'
      interval_id TEXT,              -- 仅 source='shell' 时记录
      before_exists INTEGER NOT NULL,
      before_hash TEXT,              -- SHA-256 hex，不存在时 NULL
      before_bytes BLOB,             -- 小于快照上限时存原文用于精准回滚
      after_exists INTEGER NOT NULL,
      after_hash TEXT,
      after_bytes BLOB,
      created_at INTEGER NOT NULL
  );
  CREATE INDEX IF NOT EXISTS idx_change_ledger_ws_seq
      ON agent_change_ledger(workspace, seq);
  CREATE INDEX IF NOT EXISTS idx_change_ledger_task_turn
      ON agent_change_ledger(task_id, turn, seq);
  ```

### 1.2 取消 Shell 生命周期全局写锁，改为“瞬时快照 + 账本对账”
1. **`shell_process.rs` (`begin_interval` / `finish_interval`)**：
   - **`ExecutionBoundary::ReadOnly` 的只读命令直接跳过写区间**：只读沙箱本就无法改写工作区文件，不再进入 `shell_begin` / `shell_finish`，也不占用任何锁或表记录。
   - **非只读命令不再全程持有 `MutationGuard::Workspace`**：
     - `shell_begin`：记录当前账本高水位 `start_seq = MAX(seq)`（针对该 `workspace`）并拍 `before` 快照，随后**立刻释放短锁**，不再在 shell 运行期间锁死全工作区；
     - `agent_shell_change_intervals` 表主键从 `PRIMARY KEY(workspace)` 升级为 `PRIMARY KEY(token)`（并建 `(workspace, task_id, turn)` 索引，兼容存量库自动迁移），允许同一工作区多个任务各自独立追踪 shell 区间，不再因跨会话 `exec_command` 互相报 `SHELL_BUSY`。
   - **`shell_finish` 按账本对账（Ledger Reconciliation）**：
     - 拍完 `after` 快照后，查询该 `workspace` 在 `seq > start_seq` 期间所有 `source = 'tool'` 的账本记录，按 `path` 取最后一条账本的 `after_hash`（与 `after_exists`）；
     - 对 `before` vs `after` 中发生变化的每个 `path`：
       - **若当前磁盘 `after` 哈希 == 该路径在区间内最新 `tool` 账本的 `after_hash`**（且 `exists` 一致）：说明该改动完全由工具编辑产生并已记账，shell 对账**自动扣除**，不重复归属、不触发 `managed_files` 基线冲突；
       - **若该路径无工具账本记录，或当前 `after` 哈希 != 最新工具账本的 `after_hash`**：说明 shell 确实改写了该文件，将其残余变更写入 `agent_file_changes` 与 `agent_change_ledger(source = 'shell')`；若与同区间工具编辑命中同一文件（出现交叉修改），在 `agent_file_changes` 标记 `conflicted = 1`（或标记 `incomplete`）以保障 Undo 安全，但**绝不阻塞或打断模型执行**。

### 1.3 精准放宽 `ensure_idle`（仅保留破坏性全仓回滚保护）
- [src/runtime/single_agent/host_tools.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/runtime/single_agent/host_tools.rs)：
  - 移除常规文件编辑（`write_file`、`edit_file`、`replace_range`、`batch_edits`、`delete_path`、`rename_path`、`create_dir`）对 `shell_process::ensure_idle()` 的硬拦截；
  - 仅在执行整仓破坏性操作（如 `undo` / `rewind` / `git_restore`）时，才要求确认相关工作区无活跃写型 shell（且在报错前给本会话即将退出的命令最多 2～3 秒宽限等待，避免差几百毫秒误炸）。
- 同时**严格保留**跨会话 shell 控制隔离：`SHELL_SESSION_SCOPE_DENIED` 不可动摇，任务 B 绝对不能对任务 A 的 shell 执行 `write_stdin` 或 `kill_session`。

### 1.4 消除“1 秒切后台”放大器
- [src/tools/shell_process.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/tools/shell_process.rs) 第 271 行：
  - 将 `exec_command` / `write_stdin` 未显式传 `yield_time_ms` 时的默认值从 `1_000` ms 提高到 **`10_000` ms（10 秒）**（上限仍维持 `30_000` ms；调用方显式传 `0`、`20` 等小值时严格按传入值执行，确保所有异步流式测试不受影响）。
  - 同步更新 `exec_command` 工具描述中的默认等待说明。

---

## 2. 涉及文件清单

1. [src/tools/workspace_changes.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/tools/workspace_changes.rs)
   - 新增 `agent_change_ledger` 表结构与自动建表/迁移（将 `agent_shell_change_intervals` 迁为 `token PRIMARY KEY`）；
   - 在 `execute_tracked` / 单文件写入落账处追加 `source='tool'` 账本记录（通过 `db_writer::write_async`）；
   - `ShellSnapshot` 记录 `start_seq: i64`，`shell_finish` 实现按 `agent_change_ledger` 扣除已记账工具变更的对账逻辑；
   - `undo` / `summary` 兼容新旧表与 `agent_change_ledger`（保持既有 Undo 行为与 `workspace/changes` 事件格式不破坏前端）。
2. [src/tools/shell_process.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/tools/shell_process.rs)
   - `begin_interval` 不再长持 `MutationGuard::Workspace`；
   - `ExecutionBoundary::ReadOnly` 命令跳过 `begin_interval` / `finish_interval`；
   - 默认 `yield_time_ms` 从 `1000` 调整为 `10_000`；
   - 更新 `sessions_poll_stdin_kill_tree_cancel_and_enforce_ownership` 中第 1189 行的旧断言：保留 `SHELL_SESSION_SCOPE_DENIED` 断言，将跨任务 `exec_command` 断言更新为允许并发执行且各自独立追踪。
3. [src/runtime/single_agent/host_tools.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/runtime/single_agent/host_tools.rs)
   - 第 464–466 行移除对常规单文件编辑工具的 `shell_process::ensure_idle` 拦截；
   - 保证同文件路径并发写仍受 `FILE_MUTATIONS` 与 `ResourceClaim` 串行保护。
4. [src/server/parts/mcp_tests.inc.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/server/parts/mcp_tests.inc.rs)
   - 第 173–175 行同步更新旧的跨会话 `SHELL_BUSY` 断言，保留 `SHELL_SESSION_SCOPE_DENIED` 安全断言，并验证跨会话 `exec_command` 并发互不阻断。

---

## 3. 验收与测试要求

1. **新增核心回归测试**（在 `workspace_changes.rs` 与 `shell_process.rs`）：
   - **后台 shell 运行中编辑文件不报 `SHELL_BUSY`**：启动一个常驻/等待 stdin 的 shell（如模拟 `npm run dev` 或 `read`），在 shell 存活期间用 `execute_tracked` / `write_file` / `edit_file` 修改文件，断言修改立刻成功落盘、不报 `SHELL_BUSY`；随后结束 shell，断言 `shell_finish` 通过 WAL 账本扣除工具修改，变更归属与 Undo 准确无误；
   - **shell 与工具改不同文件互不干扰**：shell 改 `a.txt`、工具改 `b.txt`，两者均准确记录在各自的归属里；
   - **ReadOnly 命令零锁开销**：`ExecutionBoundary::ReadOnly` 启动的 shell 不写入 `agent_shell_change_intervals`、不阻塞任何操作；
   - **跨任务并发安全**：任务 B 不能 `write_stdin`/`kill_session` 任务 A 的 shell（仍报 `SHELL_SESSION_SCOPE_DENIED`），但任务 B 可以正常在同工作区 `write_file` 和 `exec_command`；
   - **默认 `yield_time_ms = 10_000`**：耗时 1.5s 的短命令不传 `yield_time_ms` 时同步完成返回 `running: false`，不再残留后台 session。
2. **全量相关测试 0 警告通过**：
   - `cargo test --bin codex-workspace-mcp -- workspace_changes shell_process mcp_tests tool_wiring_invariants`
3. **执行约束**：
   - 分支严格保持 `feat/context-as-code-v2`，**禁止碰 `master`**；
   - 编译前用 `df -h .` 确认磁盘可用 > 20 GB；
   - **不要终止或重启当前运行中的 `grow-desktop` 进程**。
