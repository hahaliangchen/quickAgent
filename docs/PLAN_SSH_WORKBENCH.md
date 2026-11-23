# QuickAgent 内置远程终端与 SFTP 运维工作台（SSH Workbench）实施计划

> 目标仓库：`/home/goodtime/Documents/project/codex-workspace-mcp`（分支 `feat/context-as-code-v2`，**禁止操作 `master`**）
> 模块定位：作为 QuickAgent 的内置程序员核心工作台（与 `git` 内置插件平级），分别在后端 `src/server/builtin_plugins/ssh/` 与前端 `frontend/src/builtin_plugins/ssh/` 单开独立目录，提供 **Xshell 级多标签远程终端 + SFTP 目录树拖拽上传 + AI 会话/工作区联动记忆 + 8 个 `ssh__*` AI 运维工具**。

---

## 1. 目录隔离规划

- **后端独立目录**：`src/server/builtin_plugins/ssh/`
  - `plugin.json`：内置插件清单（`id: "ssh"`, `pages: [{ id: "workbench", title: "远程终端", entry: "ssh", renderer: "module" }]`）
  - `SKILL.md`：给 AI 的简洁运维工具使用规范与安全守则
  - `tools.json`：8 个 `ssh__*` 工具 JSON Schema
  - `mod.rs`：内置插件注册（对齐 `builtin_plugins/git/mod.rs`）、NativeContributions 组装
  - `store.rs`：SQLite 表 `ssh_servers` / `ssh_usage_logs` + `0600` 独立凭据存储 `ssh-credentials.json`（零明文泄露）
  - `client.rs`：基于 `russh` 的 SSH2 认证（`ssh-agent` / 私钥文件 / 密码）、TOFU Host Key 指纹校验、远程命令执行
  - `sftp.rs`：基于 `russh-sftp` 的目录列举、流式上传/下载、文件读写、mkdir/rename/delete
  - `pty.rs`：基于 `axum::extract::ws` 的交互式 PTY 终端会话管理（支持多标签并发、`resize`、心跳、断线清理）
  - `routes.rs`：供前端工作台调用的 REST + WebSocket 路由（挂载在 `/agent/ssh/...`）
- **前端独立目录**：`frontend/src/builtin_plugins/ssh/`
  - `index.ts`：注册到 `frontend/src/builtin_plugins/index.ts`
  - `page.tsx`：三栏工作台主壳（左：服务器列表与关联会话；中：多标签 `xterm.js` 终端；右/下可切换：SFTP 远程目录树与拖拽上传区）
  - `TerminalPane.tsx`：`xterm.js` + `FitAddon` + `WebLinksAddon` 封装（亮暗双主题、复制粘贴、resize 防抖、一键“发给 AI 诊断”）
  - `SftpExplorer.tsx`：远程目录面包屑、文件列表、本地文件拖拽上传（进度条 + 取消）、下载、新建文件夹、重命名/删除、文本只读预览弹窗
  - `ServerModal.tsx`：新增/编辑服务器表单（支持测试连接、查看/重置 Host Key 指纹、标记 `dev/staging/prod`）
  - `api.ts` & `style.module.css`
- **宿主极小胶水**：
  - `Cargo.toml`：`axum = { version = "0.8.9", features = ["ws"] }`，新增纯 Rust 依赖 `russh`、`russh-sftp`（无 C `libssh2` / OpenSSL 编译依赖）
  - `frontend/package.json`：新增 `@xterm/xterm`、`@xterm/addon-fit`、`@xterm/addon-web-links`
  - `src/server/builtin_plugins/mod.rs` + `src/server/ai_proxy.rs`：挂载 `ssh::package()?` 与 `/agent/ssh/*` 路由（受既有 `desktop_origin_guard` + `local_http_guard` 保护）
  - `frontend/src/components/CodexDock.tsx` + `frontend/src/App.tsx`：在左侧导航栏 Git 图标下方增加**远程终端（SSH）**入口图标，并复用右侧 `CodeChatDock`

---

## 2. 安全红线（第一天落地，不可降级）

1. **凭据绝对与模型/前端隔离**：
   - 服务器密码、私钥加密口令仅存放在宿主配置目录下的 `ssh-credentials.json`（Unix 权限 `0600`），**绝不写入 SQLite、绝不通过任何 GET 接口明文返回前端、绝不进入 AI 工具参数或返回值**；
   - 前端列表仅显示 `has_password: bool` / `has_passphrase: bool`；AI 工具全部通过稳定 `server_id` 引用服务器。
2. **Host Key 指纹 TOFU 校验**：
   - 首次连接记录服务器公钥 SHA-256 指纹至 `ssh_servers.host_key_fingerprint`；
   - 后续连接若指纹不一致，**立即中断并返回明确的 `HOST_KEY_MISMATCH` 错误**（展示旧指纹与新指纹），仅允许用户在 UI 服务器配置弹窗中显式点击“信任新指纹”后才可重新连接。
3. **环境分级与生产服保护**：
   - 每台服务器声明 `env_tier`: `"dev" | "staging" | "prod"`（默认 `"dev"`）以及 `require_approval_for_write: bool`（`prod` 默认 `true`）；
   - AI 只读工具声明为 `ToolPermission::ReadOnly`；写入/执行工具声明为 `ToolPermission::Execution` / `WorkspaceWrite`；当目标服务器为 `prod` 或开启写入审批时，强制要求用户人工审批确认（清晰展示目标服务器别名/Host + 完整命令或路径），禁止静默执行。
4. **本地路径沙箱（`ssh__upload` / `ssh__download`）**：
   - 本地文件路径严格经过 `cap_std` / `workspace_path` 校验在当前任务工作区内，禁止 AI 利用 `ssh__upload` 把宿主 `~/.ssh/id_rsa` 或系统文件外传到远程服务器。

---

## 3. 数据模型与 AI 工具契约

### 3.1 SQLite 表结构（由 `ssh/store.rs` 按路径去重初始化一次）
- `ssh_servers`：`id TEXT PRIMARY KEY`、`name`、`host`、`port DEFAULT 22`、`username`、`auth_type`（`agent` | `key_file` | `password`）、`key_path`、`env_tier DEFAULT 'dev'`、`require_approval DEFAULT 0`、`default_dir`、`tags_json`、`notes`、`host_key_fingerprint`、`last_connected_at`、`created_at`、`updated_at`。
- `ssh_usage_logs`：`id`、`server_id`（外键级联删）、`task_id`（关联的 QuickAgent 会话 ID）、`workspace_root`（关联的本地项目路径）、`action`（`terminal` | `exec` | `upload` | `download` | `write_file` | `read_file`）、`remote_path`、`summary`、`created_at`；建立 `(server_id, created_at DESC)` 与 `(workspace_root, created_at DESC)` 索引。

### 3.2 8 个内置 AI 工具（`ssh__*`，挂载在 `ssh` 命名空间）
1. `ssh__list_servers`（ReadOnly）：列出已登记服务器（按当前工作区最近使用优先排序），附带环境等级、默认目录、标签、以及最近关联的会话 ID/标题与项目路径（不含任何凭据）。
2. `ssh__server_history`（ReadOnly）：查询指定 `server_id` 最近 N 条使用记录（涉及会话 `task_id`、工作区、远程目录与操作摘要）。
3. `ssh__exec`（Execution）：在指定 `server_id` 执行远程命令（支持 `cwd`、`timeout_secs` 默认 60 上限 300），返回 `exit_code`、截断安全的 `stdout`/`stderr` 并记录 `ssh_usage_logs`。
4. `ssh__list_dir`（ReadOnly）：通过 SFTP 列举远程目录（分页防刷屏）。
5. `ssh__read_file`（ReadOnly）：通过 SFTP 按行分页读取远程 UTF-8 文本文件（`start_line`/`end_line`/`max_chars`）。
6. `ssh__write_file`（WorkspaceWrite）：通过 SFTP 原子写入远程文本文件（先写同目录临时文件再 rename，可选保留 `.bak`）。
7. `ssh__upload`（WorkspaceWrite）：将当前本地工作区内文件（经安全边界校验）流式 SFTP 上传至远程目标路径（支持自动建父目录与覆盖开关）。
8. `ssh__tail_log`（ReadOnly）：高效抓取远程文件尾部 N 行或按关键词过滤日志。

---

## 4. 分阶段实施路线

### Phase S1：Rust 核心存储、SSH/SFTP 引擎与内嵌测试服务端
- `Cargo.toml` 开启 `axum/ws`，接入纯 Rust `russh` + `russh-sftp`；
- 实现 `store.rs`（`ssh_servers` + `ssh_usage_logs` + `0600` 凭据隔离）、`client.rs`（`agent`/私钥/密码认证 + TOFU 指纹 + `exec`）与 `sftp.rs`（目录/流式读写/上传下载）；
- 编写内嵌 `russh::server` 本地测试服务器，零外部依赖跑通连接、指纹变更拦截、命令执行与 SFTP 大文件流式读写测试。

### Phase S2：8 个 `ssh__*` AI 工具注册、会话联动与生产服审批保护
- 编写 `plugin.json`、`SKILL.md`、`tools.json` 与 `mod.rs`，注册到 `builtin_plugins::packages()`；
- 打通 `task_id` 与 `workspace_root` 自动写入 `ssh_usage_logs`，实现 `ssh__list_servers` 按当前工作区优先排序并附带关联会话标题；
- 接通 `prod` / `require_approval` 强制审批与 `ssh__upload` 本地工作区防越权校验，补齐全部 8 个工具集成测试。

### Phase S3：REST + WebSocket PTY 接口与流式 SFTP 上传/下载接口
- 在 `routes.rs` 实现 `/agent/ssh/servers`（CRUD + 测试连接 + 重置指纹 + 关联会话查询）、`/agent/ssh/servers/{id}/sftp/*`（列目录、mkdir、rename、delete、文本预览、禁用默认 2 MiB 上限的流式上传、流式下载）以及 `/agent/ssh/servers/{id}/terminal`（WebSocket PTY，支持双向流、`resize` 控制帧与断线清理）；
- 补齐 WebSocket PTY 真实交互与 24 MiB SFTP 流式上传下载自动化测试。

### Phase S4：前端 Xshell 级工作台 UI、左侧 Dock 入口与真浏览器验收
- 在 `frontend/src/builtin_plugins/ssh/` 实现服务器侧栏（含最近关联会话一键跳转）、`xterm.js` 多标签交互终端（亮暗主题同步、resize、“发给 AI 诊断”快捷按钮）、SFTP 远程目录树（面包屑、多文件拖拽上传进度条与取消、下载、文本只读预览）；
- 在 `CodexDock.tsx` 与 `App.tsx` 接入左侧独立终端图标，右侧挂载 `CodeChatDock` 实现左终端+目录树、右 AI 对话部署；
- Playwright 真浏览器测试（连内嵌测试 SSH 服务端）：创建服务器 → 打开终端执行命令校验回显 → SFTP 目录浏览与拖拽上传 → 核对关联会话与 0 控制台报错。

---

## 5. 验收与执行约束
- 分支严格保持 `feat/context-as-code-v2`，**禁止碰 `master`**；
- 编译前用 `df -h .` 确认磁盘可用 > 20 GB；
- 全部 Rust 测试 0 警告通过、`npm --prefix frontend run typecheck` 0 错误、Playwright 端到端通过；
- **不要终止正在运行的 `grow-desktop` 进程**，完成后由宿主统一构建 release、同步桌面二进制并重启实测。

## 6. 实施范围与使用说明

- **S1 / S3**：已接入 Rust SSH/SFTP 客户端，连接时先校验主机指纹，再调用认证。首次连接使用 TOFU；更换指纹须核对旧、新两项。REST 与 PTY 仅挂载在回环监听器上；浏览器终端需精确同源及单次票据。
- **S2（完整 8 个 AI 远程运维工具 + 安全护栏）**：
  - 只读工具（`ToolPermission::ReadOnly`）：`ssh__list_servers`（按当前工作区最近使用优先排序，附最近关联会话，不含凭据）、`ssh__server_history`、`ssh__list_dir`、`ssh__read_file`、`ssh__tail_log`。
  - 远程变更与执行工具（`ToolPermission::Execution`）：`ssh__exec`、`ssh__write_file`（支持原子写入与 `.bak` 备份）、`ssh__upload`（强制使用 `cap_std` + `open_dir_nofollow` + `FollowSymlinks::No` 锁定在当前本地工作区，拒绝 `..`、越界绝对路径和符号链接逃逸）。
  - `prod` 或 `require_approval=true` 服务器：无论宿主是否处于 `FullAccess` 模式，所有远程变更工具（`ssh__exec`、`ssh__write_file`、`ssh__upload`）及人工终端/SFTP 写入均强制弹出单次人工审批（`once_only`），不可被历史会话授权或 AI 自动审查绕过；所有 AI 远程操作均将 `task_id`、`workspace_root` 与 `remote_path` 写入 `ssh_usage_logs`。
- **S4**：左侧 Dock 提供远程终端入口，内有服务器档案、多标签 PTY 和 SFTP。只有用户明确选择的终端文本才可加入代码问答草稿，不自动发送。历史可跳回关联会话。
- 主机、端口、账号及认证方式不可原地更换，以免新端点复用旧信任或凭据；地址变更时新建档案。非 Unix 不持久化密码或口令。没有将工作台对外发布，也未重启现有桌面进程。

### 本地验收

- `cargo test`：包含内嵌回环 SSH 服务、认证前拒绝指纹变化、PTY 输入/回显/resize、审批拒绝与单次票据、24 MiB 文件逐字节往返。
- `npm --prefix frontend run build` 后运行 `npm --prefix frontend run test:ssh`；首次运行需在 `frontend` 中执行 `npx playwright install chromium`。测试启动隔离的 Agent/SSH 实例，不访问外部服务器。
