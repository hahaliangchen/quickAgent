# 跨平台原生库方法补齐方案（进一步压缩 Shell 依赖）

> **目标分支**：`feat/context-as-code-v2`（严禁改动 `master`）
> **设计原则**：沿用 Capability Security（能力最小化）与零新增 Cargo 依赖原则（复用现有 `reqwest`、`libc`、`windows-sys`、`std::fs`），把最近真实日志里占比最高的“非构建类 Shell 调用”全部收进跨平台（Linux / Windows / macOS 一致）的 Rust 结构化库方法。

---

## 背景与实测数据（最近 19 轮真实任务统计）

对 `codex_state.db` 最近 19 轮任务的 966 次 Shell 调用分类后，除去真正需要留给 `process.exec_command` 的项目构建/测试命令（~500 次 `cargo` / `npm` / `playwright` / `go test`）外，其余高频 Shell 集中在 4 个跨平台兼容性最差的场景：

1. **磁盘 / 进程 / 端口 / 命令探针（~110 次）**：`df -h`（61 次）、`command -v`（18+ 次）、`ss -ltnp`、`ps -eo` —— 这些命令在 Windows 上全部不可用。
2. **Git 跨仓与空白检查（53 次）**：`git diff --check`（28 次）、`git -C /other/repo ...`（17 次）—— 现有 `git.*` 只差两个参数。
3. **本地/联调 HTTP 请求（45 次）**：`curl -s http://127.0.0.1:...`、`node -e 'fetch(...)'` —— 现有 `http` 库只有 `http.probe`（仅探活，无法读响应体或发 POST/JSON）。
4. **文件元数据与复制（~45 次）**：`stat -c '%y %s %n'`、`test -d`、`ls -la`、`cp` —— Linux 参数写法在 Windows 全挂。

---

## 总体约束（全程必须遵守）

1. **零新增 `Cargo.toml` 依赖**：严格复用现有 `reqwest`、`libc`（Unix）、`windows-sys`（Windows，已启用 `Win32_Storage_FileSystem` / `Win32_System_Diagnostics_ToolHelp` / `Win32_NetworkManagement_IpHelper`）和标准库，保证离线环境与 ARM64 麒麟打包不受影响。
2. **权限与安全边界**：
   - 严禁削弱任何已有防线：`SECRET_FILE` 凭据拦截、控制面端口拦截（`check_not_host_control_url_async`）、跨项目目录写入审批（`outside_write`）、插件会话与 `@virtual` 叶子隔离策略（注意：只读探针/文件元信息遵循现有 `page_allows` 与路径安全校验，写操作 `io.copy` 必须接入 `managed_files` / `workspace_changes` 基线与撤销记录）。
   - 所有新增工具必须完整接线到 **5 处**：
     1. `src/tools/tool_registry.rs`（注册表 + 所属库 + 读写/审批属性）
     2. `src/runtime/agent_program/declarations.rs`（`.d.ts` 精确类型签名 + `directory()` 描述）
     3. `src/runtime/single_agent/host_tools.rs`（执行分发 + 权限/审批/工作区变更追踪）
     4. `src/runtime/single_agent/tool_proxy.rs`（含 `tool_wiring_invariants` 覆盖）
     5. `frontend/src/components/toolSummary.ts` & `toolVisuals.tsx`（前端一行简洁中文摘要与图标）
3. **同步支持 `.promises`**：`io.*` / `git.*` 新增方法自动随现有机制暴露到 `io.promises.*` / `git.promises.*`。

---

## 分阶段实施清单

### Phase 1：`git.*` 增强（`repo` 跨仓支持 + `diff --check`）

- **目标文件**：
  - `src/tools/parts/tools_workspace.inc.rs`（`git_status` / `git_diff` / `git_log`）
  - `src/tools/git_tools.rs`（`git_blame` / `git_commit` / `git_restore`）
  - `src/tools/workspace.rs`（对应 Request/Response 结构体与 schema）
  - `src/runtime/single_agent/host_tools.rs`（跨仓写操作 `git_commit` / `git_restore` 接入现有 `outside_write` 审批）
  - `src/runtime/agent_program/declarations.rs`
- **具体能力**：
  1. 为 `git.status`、`git.diff`、`git.log`、`git.blame`、`git.commit`、`git.restore` 增加可选参数 `repo?: string`：
     - 缺省为当前工作区根目录；
     - 传入相对路径或绝对路径时，校验其为存在的目录并作为该次 git 调用的 `current_dir`（绝不拼 shell 字符串，仍用 `Command::new("git")` 静态 argv）；
     - 读操作（`status` / `diff` / `log` / `blame`）允许读取任意存在的外部仓库目录（与 `io.read` 一致，禁止指向敏感目录或 `.codex-workspace-mcp`）；
     - 写操作（`commit` / `restore`）当 `repo` 位于当前工作区外时，非 `full_access` 必须触发 `outside_write` 目录审批。
  2. 为 `git.diff` 增加可选参数 `check?: boolean`：
     - `check: true` 时执行 `git diff --check`（结合 `staged` / `rev_range` / `paths`），返回结构化字段：
       - `check_ok: boolean`
       - `check_issues: Array<{ path?: string; line?: number; message: string }>`
       - （兼容保留原有 `diff` / `files` / `truncated` / `stat_only` 字段）。
- **验收测试**：
  - 临时建两个独立 git 仓库（主仓 + 外部仓），验证 `repo` 跨仓 `status`/`diff`/`log`/`commit`，以及 `git.diff({ check: true })` 能检出尾随空格并返回 `check_ok: false` + 精确行号，修复后返回 `check_ok: true`。

---

### Phase 2：`http.request` 结构化 HTTP 客户端（替代 `curl` / `node -e fetch`）

- **目标文件**：
  - `src/tools/http_probe.rs`（复用已有 URL 解析、控制面端口拦截与 `reqwest` 客户端逻辑，新增 `http_request` 工具，导出 `TOOLS = &["http_probe", "http_request"]`）
  - `src/tools/tool_registry.rs`、`src/runtime/single_agent/host_tools.rs`、`src/runtime/agent_program/declarations.rs`
- **接口设计**：
  ```ts
  declare namespace http {
    function request(args: {
      url: string;
      method?: 'GET' | 'HEAD' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';
      headers?: Record<string, string>;
      body?: string | Record<string, unknown> | unknown[];
      timeout_ms?: number;       // 默认 15000，上限 60000
      max_bytes?: number;        // 默认 65536，上限 524288（流式截断，不 OOM）
      follow_redirects?: boolean; // 默认 false
    }): {
      url: string;
      method: string;
      status: number;
      ok: boolean;
      elapsed_ms: number;
      headers: Record<string, string>;
      content_type?: string;
      body: string;
      json?: unknown;
      truncated: boolean;
      bytes: number;
    };
  }
  ```
- **安全边界**：
  1. 仅允许 `http://` 与 `https://`；拒绝 URL 携带明文 `username:password@`。
  2. 强制调用 `process_sandbox::check_not_host_control_url_async(&url)`，并对重定向目标同样逐跳校验，杜绝 SSRF 触达 QuickAgent 自身控制面端口。
  3. 权限策略：
     - `ReadOnly` 模式拒绝非 `GET`/`HEAD`；
     - `Ask` / `AutoReview` 模式下：**本地 loopback（`127.0.0.1` / `localhost` / `[::1]`）的 `GET`/`HEAD` 免审批直接放行**（高频用于检查本地 dev server / API）；**写方法（`POST`/`PUT`/`PATCH`/`DELETE`）或访问外部主机**走工具审批；`FullAccess` 直接放行。
  4. 响应头脱敏：不把 `Set-Cookie` / `Authorization` 等敏感头明文打进调试日志；响应体按 `max_bytes` 流式限长读取，若 `Content-Type` 为 JSON 且未截断则自动解析填入 `json` 字段。
- **验收测试**：
  - 本地临时 `axum` / `TcpListener` 测试服务验证 `GET`/`POST JSON`/自定义 Header/超时/大响应截断；
  - 验证指向 `register_control_port` 注册的控制面端口会被拒绝；
  - 验证 `ask` 模式下本地 `GET` 免审批、非 loopback 或 `POST` 触发审批策略。

---

### Phase 3：`io.stat` + `io.copy`（替代 `stat` / `test -d` / `ls -la` / `cp`）

- **目标文件**：
  - `src/tools/workspace.rs`、`src/tools/parts/tools_workspace.inc.rs`
  - `src/tools/workspace_changes.rs`（`copy_path` 纳入 `tracked` / `written_path`，支持本轮撤销）
  - `src/tools/tool_registry.rs`、`src/runtime/single_agent/host_tools.rs`、`src/runtime/agent_program/declarations.rs`
- **接口设计**：
  ```ts
  declare namespace io {
    function stat(args: { path?: string; paths?: string[] }): {
      entries: Array<{
        path: string;
        exists: boolean;
        kind?: 'file' | 'directory' | 'symlink' | 'other';
        size_bytes?: number;
        modified_at?: string;
        modified_ms?: number;
        readonly?: boolean;
        executable?: boolean;
      }>;
    };
    function copy(args: {
      from: string;
      to: string;
      recursive?: boolean;         // 复制目录时必须显式传 true
      overwrite?: boolean;         // 默认 false，避免静默覆盖
      create_parent_dirs?: boolean; // 默认 true
    }): {
      from: string;
      to: string;
      kind: 'file' | 'directory';
      files_copied: number;
      bytes_copied: number;
      overwritten: boolean;
    };
  }
  ```
- **安全与细节**：
  - `io.stat` 支持单路径 `path` 或批量 `paths`（上限 200 条），允许检查工作区内外绝对/相对路径，但对 `SECRET_FILE`（如 `.env` / 凭据文件）与 `codex_state.db` 拒绝暴露内容并遵循现有路径校验；不跟随危险符号链接逃逸。
  - `io.copy` 复用 `file_edit` 路径校验与 `SECRET_FILE` 拦截；`to` 位于工作区外时在非 `full_access` 触发 `outside_write` 审批；单文件复制通过受控写入记录进 `workspace_changes` 与 `managed_files` 基线，递归目录复制设置安全上限（如文件数上限 5000、不跟随 symlink 循环）。
- **验收测试**：
  - 批量 `io.stat`（存在文件、目录、不存在路径、绝对路径）；
  - `io.copy` 单文件复制、冲突拒绝（`overwrite: false`）、覆盖成功（`overwrite: true`）、目录递归复制与撤销联动。

---

### Phase 4：`sys.*` 跨平台只读环境探针（替代 `df -h` / `command -v` / `ss -ltnp` / `ps`）

- **新增模块**：`src/tools/sys_info.rs`（在 `src/tools/mod.rs` 注册）
- **暴露库名**：`sys`（归入 `tool_registry.rs` 内置只读库；非 `lazy_contract_library` 或在 `declarations.rs` `directory()` 与静态声明中清晰列出；插件会话默认不暴露 `sys`）
- **接口设计**：
  ```ts
  declare namespace sys {
    /** 跨平台磁盘可用空间查询（替代 df -h）。Unix 用 libc::statvfs，Windows 用 GetDiskFreeSpaceExW。 */
    function disk_space(args?: { path?: string }): {
      path: string;
      total_bytes: number;
      available_bytes: number;
      free_bytes: number;
      available_gb: number;
      used_percent: number;
    };
    /** 跨平台批量检测可执行文件是否在 PATH 中（替代 command -v / which；Windows 自动匹配 PATHEXT）。 */
    function which(args: { names: string[] }): {
      found: Record<string, string | null>;
    };
    /** 跨平台查看本机监听 TCP 端口（替代 ss -ltnp / netstat；Linux 读 /proc/net/tcp*，Windows 用 GetExtendedTcpTable，其他平台优雅降级）。 */
    function listening_ports(args?: { port?: number; loopback_only?: boolean }): {
      ports: Array<{
        protocol: 'tcp' | 'tcp6';
        address: string;
        port: number;
        pid?: number;
        process?: string;
      }>;
      supported: boolean;
    };
    /** 跨平台只读进程列表（替代 ps -eo / pgrep；不暴露命令行敏感参数或默认截断脱敏）。 */
    function processes(args?: { filter?: string; limit?: number }): {
      processes: Array<{
        pid: number;
        ppid?: number;
        name: string;
        state?: string;
      }>;
      truncated: boolean;
      supported: boolean;
    };
  }
  ```
- **安全与跨平台细节**：
  - 全部 4 个方法均为**纯只读**、无副作用、不涉及敏感环境变量读取；
  - `processes` 仅返回 `pid` / `ppid` / 进程短名（`comm` / 可执行文件名），**绝不返回完整命令行**（避免意外泄漏命令行中的 token/密码）；
  - `listening_ports` 在 Linux 解析 `/proc/net/tcp` 与 `/proc/net/tcp6`（可选按 `/proc/<pid>/fd` 关联当前用户进程名），在 Windows 使用已有 `windows-sys` 的 `GetExtendedTcpTable`，在 macOS 若暂未实现原生 syscall 则明确返回 `supported: false` 而不报错崩溃；
  - `disk_space` 在 Unix 用 `libc::statvfs`，Windows 用 `GetDiskFreeSpaceExW`；
  - `which` 纯 Rust 遍历 `PATH`（Windows 结合 `PATHEXT`），不启动任何子进程，单次最多查 32 个命令名。
- **验收测试**：
  - `sys.disk_space({})` 返回有效 `total_bytes > 0`、`available_bytes <= total_bytes`；
  - `sys.which({ names: ["git", "__definitely_missing_binary_xyz__"] })` 准确区分存在与 `null`；
  - `sys.listening_ports`：测试内绑定一个临时 `127.0.0.1:0` `TcpListener`，验证返回结果包含该端口；
  - `sys.processes`：验证能找到当前测试进程 PID 且不泄露 cmdline。

---

### Phase 5：提示词引导、前端摘要与文档同步

- **`src/runtime/single_agent/model_call.rs` & `src/runtime/agent_program/declarations.rs`**：
  - 在 `V2_SYSTEM_PROMPT` 补一句精简引导：优先使用 `git.*`（含 `repo` / `check: true`）、`http.request`、`io.stat` / `io.copy`、`sys.*`（`disk_space` / `which` / `listening_ports` / `processes`），不再为查磁盘、探命令、测本地 HTTP、`git diff --check` 或 `stat` 动用 `process.exec_command`。
- **`AGENTS.md`**：提示可用 `sys.disk_space()` 替代 `df -h` 检查构建空间。
- **`frontend/src/components/toolSummary.ts` & `toolVisuals.tsx`**：
  - 为 `http_request`、`stat_paths`、`copy_path`、`sys_disk_space`、`sys_which`、`sys_listening_ports`、`sys_processes` 补齐中文标题、图标与单行摘要，并补前端单测（`frontend/tests/chat-process.test.mjs`）。
- **`docs/agent-design-guide.md`**：在顶部现状与库列表中补充 `sys` 库及 `http.request` / `io.stat` / `io.copy` / `git.*` 扩展说明。

---

## 最终验证清单（串行执行，禁止并行多个 `cargo`）

1. 先检查磁盘可用空间（> 20 GB）。
2. Rust 校验与测试：
   - `cargo test --bin codex-workspace-mcp -- --test-threads=2`
   - `cargo check --bin codex-workspace-mcp --features internal-daily-report`
   - 确认 `tool_wiring_invariants` 全部通过、新增测试覆盖全部 4 组能力与权限边界，且无新增 `dead_code` / `unused` 告警。
3. 前端校验与测试：
   - `npm --prefix frontend run typecheck && npm --prefix frontend run build`
   - `node --test frontend/tests/chat-process.test.mjs`
4. `git diff --check` 零告警；汇报改动与实测结果，**不要擅自 commit 或重启桌面端**。
