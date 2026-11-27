# 计划：观察者蓝图自动应用与蓝图工作台可视化重连优化

> 分支：`feat/context-as-code-v2`  
> 目标：打通「观察者任务收尾提炼 → 自动生效到项目蓝图 → 下一轮新会话零等待吃到上下文 → 蓝图工作台专注优化整理与可视化重新连线」的完整闭环。

---

## 1. 背景与现状痛点

在当前实现（提交 `82f6fce`）中，独立只读观察者（`src/server/builtin_plugins/blueprint/observer.rs`）已经能在主会话结束时稳定输出结构化 `blueprint-note`，但在真实项目（如 `rag-go`）连续使用中暴露出 4 个明显断层：

1. **停在收件箱吃灰，下一轮吃不到上下文**  
   观察者产出的 `blueprint-note` 目前只写入 `.quickagent/blueprint/inbox.json`（`status: "pending"`），必须人工进入蓝图工作台点击「交给 AI 融合」才会生成 `manifest.json` / `playbooks/` / `graphs/`。结果下一轮新会话依然处于零蓝图状态（例如 `rag-go` 新会话不知道 `loadRuntimeAsset` 优先读磁盘 `web/index.html`，白白重编上传了 53MB 二进制）。
2. **同一任务多轮收尾产生重复条目**  
   目前收件箱去重仅按 `(source_task_id, source_seq)`，同一主会话多轮结束后会在 `inbox.json` 中产生多条同主题重复线索（如 `seq=392` 与 `seq=533` 两条同标题记录）。
3. **字段别名漂移导致静默丢弃 + 失败误标已提交**  
   - 当模型偶尔使用近义字段（如 `area` / `anchors` / `reusable_constraints` / `verification_playbook`）时，`parseObserverBlueprintNote` 直接返回 `null` 静默丢弃；
   - `BlueprintObserverHost.tsx` 在 `fetch('/api/blueprint/inbox')` 成功前就把 `submitKey` 写进 `localStorage`，一旦请求失败既不提示也不会重试。
4. **蓝图工作台交互重心偏“审草稿”而非“调架构/重连线”**  
   用户希望观察者产出的经验**默认直接生效**，而蓝图工作台的定位应该是**可视化检视、合并优化、增删属性锚点、拖拽重新连线（类方法调用链）与清理过期节点**。

---

## 2. 目标架构与职责边界

### 2.1 角色分工

| 角色 | 定位 | 能力与边界 |
|---|---|---|
| **蓝图观察者（Observer）** | 自动沉淀与即时生效 | 保持**能力层严格只读**（绝不授予业务源码写权限）；任务收尾产出结构化 `blueprint-note`，由**宿主后端确定性地自动落盘生效**到 `.quickagent/blueprint/`。 |
| **主会话 Agent** | 自动消费蓝图 | 新会话 Turn 1 自动通过 `workspace_blueprint_tail` 获取最新环境/红线/最短路径，并可通过 `const bp = blueprint.open({})` / `bp.find(keyword)` 沿类方法链探索。 |
| **蓝图工作台（Blueprint Workbench）** | 可视化优化与重新连线 | 面对**已经生效**的蓝图提供：画布类卡片拖拽连线/改边/删边、属性锚点编辑、重复条目一键整理、标星锁定（Pin）、撤销自动应用（Undo）与过期锚点核验。 |

### 2.2 数据流变化

```text
旧链路：
Observer 输出 blueprint-note
  -> POST /api/blueprint/inbox (status=pending)
  -> [人工进工作台点「交给 AI 融合」]
  -> 写入 playbooks/ & graphs/
  -> 新会话生效

新链路（默认自动应用）：
Observer 输出 blueprint-note
  -> POST /api/blueprint/apply-observer-note（确定性合并 + 路径/噪音清洗）
  -> 自动 upsert playbooks/<id>.json & 可选增量更新 entities.json / graphs/<id>.json
  -> inbox.json 记录 status=applied + applied_targets（用于工作台审计与一键 Undo）
  -> 下一轮新会话立即通过 workspace_blueprint_tail / blueprint.open({}) 吃到上下文
  -> 用户随时进蓝图工作台做「整理优化 / 拖拽重新连线 / Pin 锁定 / 撤销」
```

---

## 3. 分阶段实施计划

### Phase 1：后端确定性自动应用引擎与幂等去重

**涉及文件**：
- `src/server/builtin_plugins/blueprint/model.rs`
- `src/server/builtin_plugins/blueprint/store.rs`
- `src/server/builtin_plugins/blueprint/routes.rs`
- `src/server/builtin_plugins/blueprint/mod.rs`
- `src/server/builtin_plugins/blueprint/tests.rs`

**核心任务**：
1. **扩展数据模型（向后兼容旧 JSON）**
   - `Playbook` 新增可选元字段（`#[serde(default)]`）：
     - `source: Option<String>`（`"manual"` | `"observer_auto"` | `"ai_merged"`）
     - `source_task_id: Option<String>`
     - `auto_applied_at: Option<String>`
   - `ObserverSubmissionStatus` 新增 `Applied` 状态；`ObserverSubmission` 新增 `applied_playbook_id: Option<String>`、`applied_entity_ids: Vec<String>`、`applied_graph_id: Option<String>`，用于工作台展示来源和一键撤销。
   - `ObserverSubmissionCreate` 扩展可选结构化类关系字段（全部 `#[serde(default)]`）：
     - `classes: Vec<ObserverClassHint>`（`name`, `summary`, `properties: Vec<EntityProperty>`, `methods: Vec<String>`）
     - `chains: Vec<ObserverChainHint>`（`from_class`, `from_method`, `to_class`, `to_method`, `label`, `invariant`, `severity`）

2. **实现 `Store::apply_observer_note(input)` 确定性落盘引擎**
   - **路径与噪音清洗（跨平台）**：
     - 统一经 `normalize_rel_path`（POSIX `/`）和 `strip_verbatim_prefix` 处理，拒绝 Windows 盘符绝对路径、父目录穿越 `..` 及明显临时产物（`tmp/`、`*.part-*`、`*.tmp`、`*.log`、`.staging/`）。
   - **幂等去重与增量合并**：
     - 匹配优先级：
       1. 已存在同 `source_task_id` 且标题归一化相同（或高重合）的 Playbook；
       2. 或已有 Playbook 标题归一化相同（`slug(title)` 一致）；
       3. 否则生成稳定 ID `pb-obs-<slug_or_hash>` 创建新 Playbook。
     - 命中已有 Playbook 时做**集合并集 + 新摘要覆盖**：保留 `pinned` 状态，合并去重 `anchor_files` / `environment` / `pitfalls` / `verify_commands`，刷新 `summary`、`trigger`、`updated_at`。
   - **自动初始化 `manifest.json`**：
     - 首次自动应用时自动创建 `.quickagent/blueprint/manifest.json`、`entities.json`、`playbooks/`，确保 `workspace_has_blueprint(root)` 立刻为 `true`，并失效 `TAIL_CACHE` 缓存。
   - **可选类属性与方法链增量落图**：
     - 若 `input.classes` / `input.chains` 非空，按类名归一化匹配已有 `Entity`（增量合并 `properties` 和 `ports`，绝不覆盖用户手动标定或已有布局坐标），缺失边则追加到匹配画布或默认 `auto-observed` 画布。
   - **防膨胀上限**：
     - 对 `source == "observer_auto"` 且 `pinned == false` 的 Playbook 设定上限（如默认 30 条），超限时自动归档最久未更新的非标星条目，保证 `workspace_blueprint_tail` 的提示词体积始终紧凑。
   - **审计与一键撤销（Undo）**：
     - 同步在 `inbox.json` 写入/更新对应记录为 `status: "applied"`（按 `(source_task_id, normalized_title)` 去重），并新增 `POST /api/blueprint/inbox/{id}/undo`：删除由该条新建的未标星 Playbook/增量边并把记录标记为 `dismissed`。

3. **新增 HTTP 路由**
   - `POST /api/blueprint/apply-observer-note?workspace_root=...`：执行 `store.apply_observer_note(args)` 并返回 `{ submission, playbook, created, updated_entities, updated_graph_id }`。
   - `POST /api/blueprint/inbox/{id}/undo?workspace_root=...`：撤销自动应用。
   - `POST /api/blueprint/inbox/apply-all?workspace_root=...`：将历史存量 `pending` 条目批量自动应用（方便一键消化如 `rag-go` 现有的 6 条待审线索）。

---

### Phase 2：观察者协议增强与前端自动应用闭环

**涉及文件**：
- `src/server/builtin_plugins/blueprint/observer.rs`
- `frontend/src/experimental/blueprintObserver/observerProtocol.ts`
- `frontend/src/experimental/blueprintObserver/BlueprintObserverHost.tsx`
- `frontend/tests/blueprint-observer.test.mjs`

**核心任务**：
1. **观察者提示词补充可选类/链摘要**
   - 在 `observer.rs` 的 `blueprint-note` 示例中保持极简必填字段（`title`、`trigger`、`summary`、`anchor_files`、`environment`、`pitfalls`、`verify_commands`），并说明若本轮摸清了关键代码加载/调用链（如 `Server.loadRuntimeAsset -> DiskAsset.read`），可顺手附可选的 `classes` / `chains` 数组。
2. **增强 `parseObserverBlueprintNote` 容错与别名兼容**
   - 兼容常见字段别名：
     - `title` ← `title | area | topic | name`
     - `summary` ← `summary | conclusion | description`
     - `anchor_files` ← `anchor_files | anchors | files`
     - `environment` ← `environment | reusable_constraints | constraints`
     - `verify_commands` ← `verify_commands | verification_playbook | verify`
   - 解析可选 `classes` 和 `chains` 结构并做类型校验。
3. **默认自动应用 + 修复 `localStorage` 提交时序**
   - `ObserverPrefs` 新增 `autoApplyBlueprint: boolean`（**默认 `true`**），在观察者面板顶部/设置中可切换「自动应用到蓝图 / 仅入收件箱」。
   - `BlueprintObserverHost.tsx` 收到 `blueprint-note` 后：
     - 当 `autoApplyBlueprint` 为 `true` 时调用 `POST /api/blueprint/apply-observer-note`；否则调用旧 `POST /api/blueprint/inbox`。
     - **仅在 `res.ok` 后**才把 `submitKey` 写入 `localStorage`（失败时不锁死去重键，并在 Toast 中显示可读错误）。
     - 成功后弹出轻提示：`已自动更新蓝图：<title>`，提供「在工作台查看 / 优化」或「撤销」快捷操作。

---

### Phase 3：蓝图工作台转型为「优化整理 + 可视化重新连线」工作台

**涉及文件**：
- `frontend/src/builtin_plugins/blueprint/types.ts`
- `frontend/src/builtin_plugins/blueprint/api.ts`
- `frontend/src/builtin_plugins/blueprint/page.tsx`
- `frontend/src/builtin_plugins/blueprint/BlueprintNode.tsx`
- `frontend/src/builtin_plugins/blueprint/InspectorDrawer.tsx`
- `frontend/src/builtin_plugins/blueprint/playbookPrompts.ts`
- `frontend/src/builtin_plugins/blueprint/style.module.css`

**核心任务**：
1. **画布可视化连线与重新连线（Re-wiring）**
   - 在 ReactFlow 画布（`page.tsx`）开启/补齐交互能力：
     - **`onConnect`**：从任意类卡片的方法输出端口（Handle）拖到另一类卡片的方法输入端口，直接创建方法调用链边并持久化到 `PUT /api/blueprint/graphs/{id}`；
     - **`onReconnect`（端点重连）**：支持拖动已有连线的起点或终点改连到其他类/方法端口，实时更新调用链；
     - **选中边编辑与删除**：点击连线后在右侧 `InspectorDrawer` 直接编辑 `label`、`invariant`（红线约束）、`severity`（`info` / `warn` / `critical`），或一键删除连线。
2. **类卡片属性锚点与方法直接编辑**
   - 在 `InspectorDrawer` 中支持直接增删改类实体的 `properties`（文件路径 / 符号 / 配置 / 环境 / 规则）与 `ports`（方法名、读写属性列表），无需每次都手写 JSON 或等 AI 改。
3. **自动沉淀条目的轻量治理（替代繁琐的收件箱审批）**
   - 左侧 Playbook 列表对 `source === "observer_auto"` 显示轻量「自动沉淀」角标，悬浮/选中支持一键：
     - **标星锁定（Pin）**：升级为高优核心约束，不参与自动淘汰；
     - **撤销（Undo）** / **删除**；
     - **AI 优化整理**：在工作台顶栏提供「一键整理蓝图」按钮，驱动 `BlueprintAgentDock` 对自动积累的 Playbook 与零散类进行合并去重、核对源码锚点、补全类方法连线并清理过期条目。
   - 若项目存在历史 `pending` 收件箱线索（如 `rag-go` 当前 6 条），在顶部显示一条轻提示条「有 N 条历史待应用线索 · 一键全部应用」，消化完毕后自动收起。

---

### Phase 4：测试覆盖与端到端实测验收

1. **后端 Rust 单测**（`cargo test --bin codex-workspace-mcp -- blueprint observer`）：
   - `apply_observer_note` 首次自动初始化蓝图并写入 Playbook；
   - 同会话/同标题多次提交幂等合并去重（不产生双份）；
   - 临时路径（`tmp/`、`*.part-*`）与越界路径（`../`、Windows `\\?\C:\...` 与反斜杠）过滤与规范化；
   - 携带 `classes` / `chains` 时增量写入 `entities.json` / `graphs/` 且保留既有手动节点坐标；
   - `undo` 准确回滚自动创建的 Playbook 且不动用户 `pinned` 条目；
   - 自动应用后 `workspace_blueprint_tail(&root)` 与 `blueprint.open({})` / `bp.find(keyword)` 立即命中。
2. **前端单测与构建**（`node --test frontend/tests/blueprint-observer.test.mjs frontend/tests/blueprint-playbook.test.mjs && npm --prefix frontend run typecheck && npm --prefix frontend run build`）：
   - `parseObserverBlueprintNote` 标准 schema 与字段别名兼容；
   - `BlueprintObserverHost` 请求失败时不误写去重缓存；
   - 画布 `onConnect` / `onReconnect` 边变更序列化正确。
3. **真实场景闭环验收（`rag-go`）**：
   - 将 `rag-go` 存量 6 条线索一键应用，并在蓝图工作台微调连线与标星；
   - 新开一个 `rag-go` 空会话，不补充任何历史背景，直接要求“更新 `web/index.html` 到 201 `:9005`”；
   - 验收新会话 Turn 1 直接从蓝图获知 `loadRuntimeAsset` 优先读磁盘与 `:9005` 隔离约束，直接走 400KB 热更最小代价路径。

---

## 4. 建议执行顺序（回家可直接按序推进）

1. **先落 Phase 1（后端 `apply_observer_note` + 去重 + Undo + 单测）**：约 30~45 分钟，先把自动生效核心引擎写稳。
2. **再落 Phase 2（前端容错解析 + 默认自动应用 + `localStorage` 修复）**：约 20 分钟，打通“会话结束即生效”。
3. **最后落 Phase 3（蓝图工作台 `onConnect` / `onReconnect` 拖拽重连 + 属性编辑 + 一键整理）**：约 45~60 分钟，把蓝图工作台打磨成真正的架构整理与重连工具。
4. **跑全量相关测试 + 重启桌面端（注意带 `--features internal-daily-report`）+ 用 `rag-go` 实测闭环**。
