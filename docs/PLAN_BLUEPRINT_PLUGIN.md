# 项目蓝图（Blueprint）内置插件施工计划（替换 `code-flow`）

> **适用仓库与分支**：`codex-workspace-mcp` 当前分支 `feat/context-as-code-v2`（**严禁触碰 `master`**）。
>
> **角色分工**：本计划作为 QuickAgent 施工规格书；完成后交由 Antigravity 逐项执行代码审查、Rust 单测、前端构建、Playwright 端到端实测与真机验收。
>
> **特别提醒**：当前工作区可能已有用户其他未提交的 Git 插件样式改动（`frontend/src/builtin_plugins/git/{Picker.tsx,page.tsx,style.module.css}`），**严禁回滚或修改这些无关文件**。

---

## 0. 全局目标与红线

1. **彻底移除 `code-flow`，用 `blueprint` 全面取代**：
   - 删除 `src/server/builtin_plugins/code_flow/` 与 `frontend/src/builtin_plugins/code-flow/`，清理所有对 `code-flow` / `read_code_flow` 的生产代码、资源清单与测试引用，替换为 `blueprint`。
2. **项目本地存储，按需启用零侵入**：
   - 蓝图数据存放在目标项目的 **`.quickagent/blueprint/`** 目录下（人类可读 Pretty JSON，按 key 稳定排序，便于 Git 审查或用户自行 `.gitignore`）。
   - **未存在 `.quickagent/blueprint/manifest.json` 的项目，对话与运行时零额外 Token 注入、零文件写入。**
3. **入口驱动（Trigger-First）+ 虚幻引擎蓝图（Unreal Blueprint）交互**：
   - 有前端页面的项目：按**页面与 UI 控件交互**作为最左列触发入口（点击按钮、拖拽上传、表单提交、快捷键等），从左到右穿透 `UI 交互 → 前端请求/状态 → 后端接口 → 核心服务 → 存储/外部依赖 → 测试守卫`。
   - 无前端的纯后端/服务/CLI/库项目：按**对外接口（HTTP/gRPC/WS）、后台 Worker/定时任务、命令行子命令/公开 API**作为最左列触发入口。
   - 混合全栈项目：前端页面为主画布 Tab，另配独立「纯接口 / 后台任务」Tab 收容无 UI 入口的协议与异步链路。
4. **双层统一：多画布入口（`graphs/*.json`）+ 共享实体库（`entities.json`）**：
   - 每张画布聚焦一个入口场景（推荐 6~15 张类卡片），避免全库糊成蜘蛛网；
   - 不同画布引用同一个 `entity_id`；一处更新端口/红线，全局生效，并支持一键反查跨画布影响面。
5. **安全边界**：
   - 所有蓝图 REST API 与源码预览接口严格校验路径在目标 `workspace_root` 内部（canonicalize 校验防 `..` 路径穿越），只允许在 `.quickagent/blueprint/` 下写入蓝图文件。
   - 不搞脆弱的重型 AST 解析器；后端锚点校验用轻量文件存在性 + 行号范围 + 符号文本匹配 + `git diff` 毫秒级完成。

---

## 1. 数据模型与目录结构（`.quickagent/blueprint/`）

```text
<workspace_root>/.quickagent/blueprint/
├── manifest.json          # 项目元数据、画布列表、last_synced_commit
├── entities.json          # 全局共享实体库（类/页面/API/服务/存储/外部依赖/测试守卫）
└── graphs/
    ├── <graph_id>.json    # 单个入口画布（画布节点坐标、方法端口到端口连线、场景红线不变量）
    └── ...
```

### 1.1 `manifest.json`
```json
{
  "version": 1,
  "project_kind": "fullstack",
  "last_synced_commit": "c55e82d",
  "updated_at": "2026-11-24T09:00:00Z",
  "graphs": [
    {
      "id": "ssh-workbench",
      "title": "SSH 远程运维工作台",
      "entry_kind": "ui_page",
      "entry_summary": "用户在 SSH 页面添加主机、操作终端、拖拽上传文件与查看 AI 实时跟随"
    }
  ]
}
```
- `project_kind` 枚举：`"fullstack" | "frontend" | "backend" | "cli" | "library"`
- `entry_kind` 枚举：`"ui_page" | "service_api" | "background_job" | "cli_command" | "custom"`

### 1.2 `entities.json`
```json
{
  "entities": [
    {
      "id": "ui.ssh_workbench",
      "name": "SshWorkbench",
      "kind": "trigger_ui",
      "traits": ["UiView", "Stateful"],
      "file_path": "frontend/src/builtin_plugins/ssh/SshWorkbench.tsx",
      "summary": "SSH 远程终端与 SFTP 主工作台",
      "ports": [
        {
          "id": "on_add_server",
          "name": "点击：添加服务器",
          "direction": "out",
          "kind": "event",
          "symbol": "handleAddServer",
          "line_start": 120,
          "line_end": 148,
          "summary": "校验表单并调用后端创建主机配置"
        }
      ]
    }
  ]
}
```
- `entity.kind` 枚举（决定虚幻蓝图卡片顶条色）：
  - `"trigger_ui"`（暖红：前端页面/控件触发）
  - `"trigger_api"`（珊瑚红：HTTP/gRPC/WS/CLI 对外入口）
  - `"trigger_job"`（玫红：定时/Worker/消息队列触发）
  - `"service"`（钴蓝：前端请求层、后端业务类/模块、Agent 工具）
  - `"store"`（翠绿：DB 表、配置文件、内存注册表、缓存）
  - `"external"`（紫罗兰：外部网关、远端主机、第三方 API）
  - `"test_guard"`（琥珀橙：关键不变量与回归测试）
- `port.direction` 枚举：`"in" | "out" | "both"`
- `port.kind` 枚举：`"event" | "method" | "route" | "tool" | "query" | "mutation" | "state" | "guard"`

### 1.3 `graphs/<graph_id>.json`
```json
{
  "id": "ssh-workbench",
  "title": "SSH 远程运维工作台",
  "entry_kind": "ui_page",
  "entry_summary": "用户在 SSH 页面添加主机、操作终端与拖拽上传",
  "invariants": [
    "SSH 凭据文件权限必须为 0600，审批卡片中凭据必须脱敏为 [redacted]",
    "prod 写入与 ssh__add_server 强制 once_only 审批"
  ],
  "nodes": [
    { "entity_id": "ui.ssh_workbench", "x": 40, "y": 80 },
    { "entity_id": "api.ssh_routes", "x": 440, "y": 80 }
  ],
  "edges": [
    {
      "id": "e1",
      "source_entity": "ui.ssh_workbench",
      "source_port": "on_add_server",
      "target_entity": "api.ssh_routes",
      "target_port": "post_servers",
      "relation": "http",
      "label": "POST /api/ssh/servers",
      "invariant": "密码仅在内存与 0600 凭据库流转",
      "severity": "critical"
    }
  ]
}
```
- `edge.relation` 枚举：`"triggers" | "http" | "calls" | "reads" | "writes" | "emits" | "must_sync" | "guards"`
- `edge.severity` 枚举：`"info" | "warning" | "critical"`（默认 `"info"`）

---

## 2. 阶段 1：Rust 后端插件 + 校验引擎 + AI 工具 + 移除 `code-flow`

### 2.1 新建 `src/server/builtin_plugins/blueprint/`
拆分清晰子模块：
- `mod.rs`：导出 `package()`、`routes` 及辅助查询（例如 `workspace_has_blueprint(root: &Path) -> bool` 与任务启动极简摘要函数）。
- `model.rs`：定义 `Manifest`、`GraphSummary`、`EntityStore`、`Entity`、`Port`、`BlueprintGraph`、`GraphNode`、`BlueprintEdge`、`PortStatus`（`ok | drifted | missing`）、`VerifyReport`、`ImpactReport` 等强类型 serde 结构与 ID 合法性校验（`graph_id` 仅允许 `[a-zA-Z0-9_-]`，防路径穿越）。
- `store.rs`：
  - 原子读写 `.quickagent/blueprint/{manifest.json, entities.json, graphs/<id>.json}`（先写临时文件再 rename，JSON pretty-print）。
  - `upsert_entities`：按 `entity.id` 合并写入；更新同名 `port.id` 或追加新端口，支持显式 `remove_entity_ids` / `replace_ports` 选项。
  - `upsert_graph`：创建或更新 `graphs/<id>.json` 并同步更新 `manifest.json` 的 `graphs` 目录与 `updated_at`；**关键保护**：若目标 `graph` 已存在且传入节点未提供 `x/y`，必须保留用户在画布上已保存的旧 `x/y` 坐标（新增节点自动计算不重叠的默认左→右分层坐标）。
  - `delete_graph`：删除画布文件并同步移除 `manifest.json` 索引。
  - `save_layout`：仅保存节点 `x/y` 坐标（前端拖动防抖调用，绝不误改实体或连线）。
- `verify.rs`（确定性、零 LLM 成本校验）：
  - 对 `entities.json` 中每个带 `file_path` 的实体与端口执行校验：
    - 文件不存在 → 实体与所有端口标为 `missing`；
    - 文件存在：若端口填了 `symbol`，检查 `line_start..=line_end`（容差 ±5 行）是否包含 `symbol` → 包含则 `ok`；若不在原行段但在文件其他行找到 `symbol` → 标为 `drifted` 并返回 `suggested_line`；若全文件找不到 `symbol` → `missing`；
    - 若端口未填 `symbol` 但填了 `line_start`：检查总行数是否越界；再结合 `git status --porcelain` 与 `git diff --name-only <last_synced_commit>..HEAD`：文件自上次同步后被修改过则标为 `drifted`，未改则 `ok`。
  - 返回 `VerifyReport`：`{ exists, head_commit, last_synced_commit, changed_files, affected_entities, affected_graphs, ok_ports, drifted_ports, missing_ports, stale: bool }`。
  - 提供 `read_source_snippet(workspace_root, file_path, line_start, line_end, context_lines)`：严格校验 canonical path 位于 `workspace_root` 内，返回带行号的源码片段供前端点击端口预览。
- `impact.rs`：
  - 输入 `entity_id` 或 `file_path`（可选 `port_id`），在所有 `graphs/*.json` 中执行有向 BFS（默认深度 3，可配），返回：
    - 匹配的实体与所属画布列表；
    - `upstream`（谁触发/调用它，含跨画布连线）；
    - `downstream`（它调用/写入/影响的模块、存储与外部系统）；
    - `invariants`（涉及画布与连线上的全部红线约束，按 `critical` 优先去重）；
    - `test_guards`（可达的全部 `kind == "test_guard"` 节点与测试入口）。
- `package.rs` / `plugin.json` / `SKILL.md`：
  - `plugin.json`：
    ```json
    {
      "version": 1,
      "id": "blueprint",
      "library": "blueprint",
      "title": "架构蓝图",
      "skill": "SKILL.md",
      "tools": [
        "blueprint__overview",
        "blueprint__read_graph",
        "blueprint__impact",
        "blueprint__verify_sync",
        "blueprint__upsert_entities",
        "blueprint__upsert_graph"
      ],
      "pages": [
        {
          "id": "workbench",
          "title": "架构蓝图",
          "entry": "blueprint.workbench",
          "renderer": "react"
        }
      ]
    }
    ```
  - 权限划分：
    - `blueprint__overview`、`blueprint__read_graph`、`blueprint__impact`、`blueprint__verify_sync` → `ToolPermission::ReadOnly`
    - `blueprint__upsert_entities`、`blueprint__upsert_graph` → `ToolPermission::WorkspaceWrite`
  - **AI 返回精简规则**：`blueprint__read_graph` 返回给 AI 时**剥离 `x/y` 画布像素坐标**，仅保留高信号结构（`entity`、`traits`、`ports` + 校验状态、`edges` + 红线、`invariants`）。
  - `SKILL.md` 写明清晰规范：
    - 何时读蓝图（修改跨层功能或共享底层前先 `blueprint.overview()` / `blueprint.impact()`）；
    - 如何做「启动分析」建图：先识别项目形态（有前端按核心页面控件触发建画布 + 纯接口/后台画布；纯后端按接口域/Worker 建画布；CLI/库按命令/公开 API 建画布），每张图控制在 6~15 张核心类卡片，只记录跨层关系、隐性耦合与红线不变量，不堆同文件碎函数；
    - 如何做「增量同步」：先 `blueprint.verify_sync()`，只针对 `changed_files` 与 `drifted`/`missing` 端口增量修正，并传入最新 `last_synced_commit`。
- `routes.rs`（挂载到 `src/server/ai_proxy.rs`，前缀 `/api/blueprint`）：
  - `GET /api/blueprint/state?workspace_root=...`：返回完整前端渲染态（`exists`, `manifest`, `entities`, `graphs` 目录, `verify` 汇总）。
  - `GET /api/blueprint/graph/{graph_id}?workspace_root=...`：返回单张画布详情 + 解析后的实体对象 + 每个端口实时校验状态。
  - `POST /api/blueprint/graph?workspace_root=...`：新建空白画布或更新画布元信息/红线。
  - `PUT /api/blueprint/graph/{graph_id}?workspace_root=...`：整图保存（前端新增/删除节点、端口连线、边标签与红线、画布红线）。
  - `DELETE /api/blueprint/graph/{graph_id}?workspace_root=...`：删除画布。
  - `PUT /api/blueprint/graph/{graph_id}/layout?workspace_root=...`：轻量仅更新节点 `x/y` 坐标。
  - `PUT /api/blueprint/entities?workspace_root=...`：新增/更新实体及其端口/Traits。
  - `DELETE /api/blueprint/entities/{entity_id}?workspace_root=...`：删除实体并级联清理各画布中引用它的节点与连线。
  - `GET /api/blueprint/impact?workspace_root=...&entity_id=...&port_id=...`：供前端点击节点/端口时实时高亮影响面与跨画布引用。
  - `GET /api/blueprint/verify?workspace_root=...`：刷新 `git diff` 与源码锚点校验结果。
  - `GET /api/blueprint/source?workspace_root=...&file_path=...&line_start=...&line_end=...`：安全读取对应端口源码片段（带行号）。

### 2.2 任务启动轻量同步提示（零未启用开销）
- 在 `src/runtime/single_agent/model_call.rs` 构建工作区动态提示时，调用 `blueprint::workspace_blueprint_hint(workspace_root)`：
  - 若 `<workspace_root>/.quickagent/blueprint/manifest.json` **不存在**：返回 `None`（不注入任何字符）；
  - 若存在：注入精简 1~2 行提示，例如：
    ```text
    [Project Blueprint] Available (.quickagent/blueprint, 4 graphs, 2 drifted ports). Before editing cross-layer code or shared modules, inspect with `blueprint.overview()` or `blueprint.impact({ file_path })` to check architecture links, invariants, and test guards.
    ```

### 2.3 彻底移除 `code-flow` 的必改清单
1. 删除目录 `src/server/builtin_plugins/code_flow/` 与 `frontend/src/builtin_plugins/code-flow/`。
2. `src/server/builtin_plugins/mod.rs`：将 `pub mod code_flow;` 替换为 `pub(crate) mod blueprint;`，`packages()` 中替换 `code_flow::package()?` 为 `blueprint::package()?`。
3. `src/server/ai_proxy.rs`：合并挂载 `crate::server::builtin_plugins::blueprint::routes::router()`。
4. `desktop/tauri.conf.json`：`bundle.resources` 中将 `"../src/server/builtin_plugins/code_flow/": "src/server/builtin_plugins/code_flow/"` 替换为 `"../src/server/builtin_plugins/blueprint/": "src/server/builtin_plugins/blueprint/"`。
5. `src/runtime/agent_program/declarations.rs`：
   - 在 `directory()` 常用插件库列表中登记 `("blueprint", "Inspect or maintain this project's trigger-first architecture blueprint (.quickagent/blueprint), trace cross-layer impact/invariants, and verify sync against git diff; discover full schemas on demand with `host.contracts({ libraries: [\"blueprint\"] })`.")`。
   - 确认 `lazy_contract_library` 自动为 `blueprint` 生成完整 TS 签名与 `inputSchema`。
6. `src/context/context_assets.rs`：将 `"read_code_flow"` 替换为 `"blueprint__overview" | "blueprint__read_graph" | "blueprint__impact" | "blueprint__verify_sync"`。
7. `src/runtime/single_agent/host_tools.rs`：将 `read_code_flow` 的只读路径检查白名单替换为上述 4 个只读 `blueprint__*` 工具（注意：`blueprint__impact` 接受 `file_path`，`workspace_root` 越界校验需保持生效）。
8. `src/model/model_catalog.rs`、`src/server/parts/mcp_tests.inc.rs`、`src/server/parts/local_api_proxy_tests.rs`、`src/server/plugin_sessions.rs`、`src/server/skill_packages.rs`：将所有测试中的 `"code-flow"` / `"read_code_flow"` 更新为 `"blueprint"` / `"blueprint__overview"`，确保全量测试绿灯且全仓库无残留 `code-flow` / `read_code_flow` 引用。

---

## 3. 阶段 2：虚幻蓝图风格 React Flow 前端 + `CodeChatDock` 联动

### 3.1 导航与顶层壳层集成
- `frontend/src/builtin_plugins/index.ts`：移除 `'code-flow'`，注册 `blueprint: () => import('./blueprint/index.ts')`。
- `frontend/src/components/CodexDock.tsx`：
  - `DockTab` 新增 `'blueprint'`（`'home' | 'skills' | 'git' | 'ssh' | 'blueprint' | 'office'`），在左侧图标栏放置清晰简洁的蓝图/节点连线图标（title: `架构蓝图 (Blueprint)`），与 `Git`、`SSH` 视觉风格一致。
- `frontend/src/App.tsx`：
  - 参照 `ssh` / `git` 的 `dockedView` 模式接入 `blueprint`：
    - 左侧为 `<PluginPageHost owner="blueprint" page="workbench" workspaceRoot={gitWorkspace} theme={dark ? 'dark' : 'light'} />`；
    - 右侧直接挂载 `<CodeChatDock key="blueprint" workspace={gitWorkspace} ... diagnostic={blueprintDraft} embeddedLauncher linkedTaskId={taskId} linkedSession={workspace.session} openSignal={blueprintChatOpenSignal} />`；
    - 提供轻量 `BlueprintContext`（类似 `SshContext`），把 `sendToAi(prompt: string)` 传给蓝图页面，点击「启动分析」「增量同步」「发给 AI 讨论」时一键打开右侧 `CodeChatDock` 并填入结构化指令！

### 3.2 蓝图工作台组件设计（`frontend/src/builtin_plugins/blueprint/`）
文件结构：
- `index.ts`：注册 `blueprint.workbench` 页面。
- `types.ts`：与后端一一对应的 TypeScript 类型。
- `api.ts`：封装 `/api/blueprint/*` 请求。
- `layout.ts`：左→右拓扑分层自动排版算法（按 `trigger_*` → `service` → `store/external/test_guard` 分列，结合入度/拓扑序排 Y 坐标，零外部重依赖即可排得工整清晰）。
- `BlueprintNode.tsx`（核心：**虚幻引擎蓝图风格多端口类卡片**）：
  - **顶部语义条**：按 `entity.kind` 着色（暖红/珊瑚红/玫红/钴蓝/翠绿/紫罗兰/琥珀橙），左侧显示图标 + 类名，右侧显示接口 `traits` 胶囊与跨画布引用计数角标（点击可查看引用该实体的其他画布）。
  - **副标题行**：精简显示 `file_path`（悬浮显示完整路径）+ 一句话职责。
  - **方法/交互端口列表**：
    - 每行一个端口，左侧挂载 `<Handle type="target" id={`in:${port.id}`} position={Position.Left} />`，右侧挂载 `<Handle type="source" id={`out:${port.id}`} position={Position.Right} />`（支持虚幻风格的圆点/箭头插槽样式，悬浮放大发光）；
    - 行内显示端口类型小徽章（`EVT`/`API`/`FN`/`DB`/`GUARD`）、端口名称、符号名（`monospace`）和实时新鲜度圆点（绿 `ok` / 黄 `drifted` / 红 `missing`）；
    - 点击端口行：在底部/侧边抽屉展开对应源码片段预览（带行号与语法高亮），并可触发「影响面高亮」；
    - 卡片底部提供轻量 `+ 添加端口` 按钮。
- `BlueprintEdge.tsx`（自定义端口连线）：
  - 平滑贝塞尔曲线（Bezier），按 `relation` 区分线型/色调（如 `http` 实线蓝、`writes` 实线绿、`must_sync` 虚线琥珀、`critical` 红线高亮发光）；
  - 连线中点显示简洁标签气泡（如 `POST /api/ssh/servers`），若带有 `invariant` 红线说明则附带红色警示小标，悬浮或点击可查看/编辑完整红线文本。
- `InspectorDrawer.tsx`：
  - 点击节点、端口或连线时展开（不占满画布，可随时关闭），支持：
    - 编辑实体名称、类型、Traits、文件路径、摘要；
    - 增删改方法端口（名称、方向、类型、符号、行号）；
    - 编辑连线关系类型、标签、红线不变量与严重级别；
    - 查看端口对应源码片段（复用现有 `CodeBlock`）。
- `page.tsx` & `style.module.css`：
  - **严格适配 Light Paper 与 Dark Ink 双主题**（使用现有 CSS 变量体系，杜绝写死刺眼纯黑/纯白，顶栏高度 `48px` 与 SSH/Git 页对齐）。
  - **空态引导（项目尚无 `.quickagent/blueprint/` 时）**：
    - 清爽展示两个直接动作按钮：
      1. **「启动全项目蓝图分析」**（自动向右侧 `CodeChatDock` 填入全项目分析提示词：先判项目形态，有前端从页面控件触发入手 + 纯接口画布，无前端从接口/任务入手，调用 `blueprint.upsert_entities` & `blueprint.upsert_graph` 生成初版蓝图）；
      2. **「手动新建空白蓝图」**（立刻初始化 `.quickagent/blueprint/` 并进入可拖拽编辑画布）。
  - **常态三区布局**：
    - **左侧边栏（约 240px，可折叠）**：
      - 画布 Tab 列表（按 `ui_page` / `service_api` / `background_job` 分组显示，支持 `+ 新建画布` / 删除）；
      - 画布红线不变量清单（可增删）；
      - 共享实体库搜索列表（点击可将已有共享实体一键「拖入/加入当前画布」）。
    - **中央画布区**：
      - 48px 顶栏：当前画布标题、项目形态标签、同步状态胶囊（`已同步` / `发现 N 处漂移`）、**「启动分析」**（支持下拉选择“全项目分析”或“分析指定模块/页面”）、**「增量同步」**、**「自动排版」**、**「影响面模式」**、**「发给 AI 讨论」**（当画布选中 1+ 节点或连线时高亮可点）、`+ 新建节点`。
      - React Flow 主画布（带 `Background`、`Controls`、`MiniMap`）：
        - 支持从任意端口圆点拖拽连线到另一个端口（`onConnect`），连线后弹出小气泡选 `relation` 与可选 `invariant`；
        - 支持拖动节点位置防抖保存（`PUT /api/blueprint/graph/{id}/layout`）；
        - 支持框选多节点（`selectionOnDrag` 或 Shift 框选）后点「发给 AI 讨论」，把选中节点、端口、连线与红线格式化为结构化上下文塞进右侧 `CodeChatDock`；
        - 支持监听右侧 AI 任务完成或定时轻量刷新（以及手动刷新按钮），AI 调 `blueprint.upsert_*` 后画布自动更新显示！

---

## 4. 验收标准与自测清单（施工方必做）

### 4.1 后端与集成测试
1. 新增 `src/server/builtin_plugins/blueprint/tests.rs`（或模块内 `#[cfg(test)]`），至少覆盖：
   - 实体与画布 CRUD 持久化（验证 `.quickagent/blueprint/` 文件结构完整）；
   - `upsert_graph` 更新连线时不覆盖用户已有节点 `x/y` 坐标；
   - `verify_sync` 准确识别 `ok` / `drifted`（符号偏移或文件修改）/ `missing`（文件或符号删除）；
   - `impact` 跨多张画布正确追溯上游触发点、下游存储、红线不变量与 `test_guard`；
   - 路径穿越攻击防护（`file_path: "../../etc/passwd"`、`graph_id: "../evil"` 均被拒绝）；
   - `workspace_blueprint_hint` 在无 `.quickagent/blueprint/manifest.json` 时返回 `None`，存在时返回精简提示；
   - `read_tool_contracts` 对 `blueprint` 返回 6 个工具的完整 TypeScript 签名与 `inputSchema`。
2. 全仓库确认零 `code-flow` / `read_code_flow` 残留引用（文档历史记录除外）。
3. 跑通相关与核心测试套件：
   - `cargo test blueprint`
   - `cargo test mcp_tests` / `cargo test local_api_proxy_tests` / `cargo test plugin_sessions` / `cargo test skill_packages` / `cargo test declarations` / `cargo test model_catalog`
   - 确保编译 **0 warning**。

### 4.2 前端类型检查、构建与 Playwright 验证
1. `npm --prefix frontend run typecheck && npm --prefix frontend run build` 0 错误通过。
2. 新增轻量 Playwright 验证脚本（如 `frontend/scripts/verify-blueprint.mjs`），在 Light 与 Dark 双主题下验证：
   - 左侧栏点击蓝图图标进入工作台，无控制台报错；
   - 创建画布、添加多端口节点、从节点 A 端口拖拽连线到节点 B 端口、编辑红线、点击「自动排版」、刷新页面后数据与坐标完整保留；
   - 点击端口预览源码正常展示；
   - 点击「发给 AI 讨论」或「启动分析」能正确唤醒右侧 `CodeChatDock` 并填入结构化草稿。

### 4.3 完成后向 Antigravity 汇报
施工完成后，请汇报：
1. 修改/新增/删除文件清单；
2. Rust 测试与前端构建/Playwright 验证结果；
3. 遗留风险或值得重点审查的设计点。
（验收前请勿自行合并到 `master`，也不要覆盖 `frontend/src/builtin_plugins/git/` 的未提交改动。）
