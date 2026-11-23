# 任务计划面板「两层子计划（Sub-steps）」实施方案

> 目标：在保持 `plan_set` / `plan_update` 现有协议与历史会话 100% 向后兼容的前提下，为任务计划面板增加**一级主步骤 → 一级子步骤（最多 2 层）**能力，支持动态追加子步骤、父子状态联动、加权进度计算与折叠/展开层次展示。

---

## 1. 数据模型与规则（严格限制 2 层）

### 1.1 `Step` 结构扩展（`src/tools/plan_tools.rs`）
- 在 `Step` 上新增可选字段：
  ```rust
  #[serde(default, skip_serializing_if = "Option::is_none")]
  parent_id: Option<String>,
  ```
- **向后兼容**：历史数据库中不含 `parent_id` 的 `steps_json` 反序列化后默认为 `None`（顶层主步骤），无需 DB 迁移，完整兼容 `rewind`。
- **强校验规则（`validate_steps`）**：
  1. 总步骤数（主步骤 + 子步骤）不超过 `MAX_STEPS`（100）；
  2. 所有 `step.id` 全局非空、唯一、长度 `<= 96` 字节；
  3. `parent_id` 若存在：
     - 去除空白后必须非空，且不得等于自身 `step.id`；
     - 必须指向同一计划中**已存在的顶层步骤**（即目标父步骤的 `parent_id` 必须为 `None`，**严禁出现第 3 层嵌套或孤儿 `parent_id`**）。

### 1.2 父子状态联动规则（保持简洁、不穿帮）
- **子步骤开始时自动推进父步骤**：通过 `plan_update` 将某子步骤改为 `in_progress` 时，若其父步骤当前为 `pending`，后端自动将父步骤状态同步改为 `in_progress`；
- **子步骤受阻时清晰反映**：若子步骤为 `blocked`，父步骤本身状态保留不强制覆盖（前端会优先提示受阻子步骤）；
- **父步骤完成时的一致性约束**：通过 `plan_update` 将某父步骤标为 `done` 时，若该父步骤下仍有未完成子步骤（非 `done`），默认拒绝报错提示模型先更新子步骤状态（或要求传参 `complete_children: true` 显式收口），避免界面出现“父项已勾绿但底下子项仍是待开始”的穿帮。

### 1.3 进度计数规则
- `completed` 与 `total` 字段继续保留在 `plan/updated` 事件及工具返回值中，语义为**顶层主步骤数**（与旧客户端和现有测试完全兼容）；
- 额外返回浮点或整型百分比辅助信息，或由前端基于 `steps` 统一按**加权公式**计算展示进度：
  - 每个顶层主步骤权重相等（各占 `1 / top_total`）；
  - 若某顶层主步骤无子步骤：完成度为 `done ? 1 : 0`；
  - 若某顶层主步骤有 `k` 个子步骤：
    - 当父步骤为 `done` 时完成度为 `1`；
    - 否则完成度为 `done_children / k`；
  - 总体百分比 `percent = Math.round((sum_weights / top_total) * 100)`，进度环随子步骤逐项完成平滑推进。

---

## 2. 后端工具与全链路注册

### 2.1 新增工具 `plan_add_substeps`（`plan.add_substeps`）
为了避免 AI 执行到某一阶段想拆细项时被迫重传整份 `plan_set`，新增增量追加工具：
- 名称：`plan_add_substeps`（JS-VM 命名空间：`plan.add_substeps`）
- 权限：`PermissionMode::ReadOnly`（与 `plan_set` / `plan_update` 一致，不改工作区文件、不触发审批）
- 入参 Schema：
  ```json
  {
    "type": "object",
    "required": ["parent_id", "steps"],
    "properties": {
      "parent_id": { "type": "string", "minLength": 1, "maxLength": 96 },
      "steps": {
        "type": "array",
        "minItems": 1,
        "maxItems": 50,
        "items": {
          "type": "object",
          "required": ["id", "title", "status"],
          "properties": {
            "id": { "type": "string", "minLength": 1, "maxLength": 96 },
            "title": { "type": "string", "minLength": 1, "maxLength": 200 },
            "status": { "enum": ["pending", "in_progress", "done", "blocked"] },
            "note": { "type": "string", "maxLength": 500 }
          }
        }
      }
    }
  }
  ```
- 行为：
  - 读取当前最新快照 `latest(&tx, task, turn)`；
  - 校验 `parent_id` 存在且为顶层步骤（`parent_id.is_none()`）；
  - 将新增子步骤自动填充 `parent_id: Some(parent_id)`，插在该父步骤最后一个已有子步骤之后（保持同一父步骤下的顺序紧凑）；
  - 若任一新子步骤为 `in_progress` 且父步骤为 `pending`，同步把父步骤推至 `in_progress`；
  - 走统一的 `validate_steps`、`INSERT ... ON CONFLICT` 落库并发送 `plan/updated` 事件；
  - 返回 `{"steps": ..., "added": ..., "completed": ..., "total": ...}`。

### 2.2 必须同步更新的全部后端接线点（缺一会导致 `tool_wiring_invariants` 失败）
1. [src/tools/plan_tools.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/tools/plan_tools.rs)：
   - `TOOLS = &["plan_set", "plan_update", "plan_add_substeps"]`
   - `step_schema()` 增加可选 `parent_id`
   - `definitions()` 更新 `plan_set` / `plan_update`（可选 `complete_children`）并新增 `plan_add_substeps`
   - `apply()` 实现 2 层校验、父子联动与 `plan_add_substeps`
2. [src/tools/tool_registry.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/tools/tool_registry.rs)：
   - 在 `plan` 命名空间下新增 `plan.add_substeps => plan_add_substeps;`（权限 `PermissionMode::ReadOnly`）
3. [src/server/agent_service/tools.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/server/agent_service/tools.rs)：
   - 补齐 `plan_add_substeps` 的输出 Schema
4. [src/runtime/agent_program/declarations.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/runtime/agent_program/declarations.rs)：
   - 更新 `plan_set`、`plan_update` 的 TypeScript 声明（加入 `parent_id?: string` 与可选 `complete_children?: boolean`），新增 `plan_add_substeps` 声明
5. [src/runtime/single_agent/tests/tool_wiring_invariants.rs](file:///home/goodtime/Documents/project/codex-workspace-mcp/src/runtime/single_agent/tests/tool_wiring_invariants.rs)：
   - 补齐 `("plan", "add_substeps", "plan_add_substeps", PermissionMode::ReadOnly, false, false)` 契约断言

---

## 3. 前端改造（`frontend/src/`）

### 3.1 [frontend/src/model.ts](file:///home/goodtime/Documents/project/codex-workspace-mcp/frontend/src/model.ts)
- `PlanStep` 增加可选字段 `parent_id?: string`；
- `latestPlan()` 的 `valid` 校验允许 `step.parent_id === undefined || typeof step.parent_id === 'string'`；
- 可新增纯函数 helper（如 `groupPlanSteps(steps)`）把扁平 `steps` 整理为 `{ topSteps: Array<PlanStep & { children: PlanStep[] }>, topCompleted, topTotal, percent }`，方便单测和组件复用（对意外出现的孤儿 `parent_id` 做降级兜底视为顶层步骤，绝不静默丢项）。

### 3.2 [frontend/src/components/PlanProgress.tsx](file:///home/goodtime/Documents/project/codex-workspace-mcp/frontend/src/components/PlanProgress.tsx) 与 [PlanProgress.module.css](file:///home/goodtime/Documents/project/codex-workspace-mcp/frontend/src/components/PlanProgress.module.css)
- **顶部概览行**：
  - 进度环百分比使用**子步骤加权 `percent`**；
  - 计数显示顶层步骤进度 `{topCompleted} / {topTotal}`；
  - 右上角折叠状态文案：若某顶层步骤下有 `blocked` 或 `in_progress` 的子步骤，展示 `父步骤标题 › 子步骤标题`（超长自然省略），让用户不展开也能看清当前细项；
  - 分段进度条按顶层步骤分段（`topTotal <= 16`）；当某顶层步骤为 `in_progress` 且存在已完成子项时，可按子项完成比例平滑呈现段内进度或维持进行中态。
- **展开态两层时间线**：
  - 顶层步骤沿用现有视觉卡片；当存在子步骤时，在标题右侧显示紧凑子进度角标（如 `2/4`）与小折叠箭头；
  - **默认展开策略**：`in_progress` 和 `blocked` 的顶层步骤默认展开其子步骤列表；`done` 和 `pending` 的顶层步骤默认折叠子步骤，用户点击可手动切换展开/收起（用本地 `Record<string, boolean>` 记录手动覆写态）；
  - 子步骤渲染在父步骤下方缩进的细竖轨上（字号与图标比主步骤略轻一级，保持浅色/暗色主题一致，不喧宾夺主）。

---

## 4. 验收与测试要求

1. **Rust 单元测试**（在 `src/tools/plan_tools.rs` 新增）：
   - `plan_set` 支持带 `parent_id` 的两层步骤，拒绝第 3 层嵌套、指向不存在父节点、自引用和重复 ID；
   - `plan_add_substeps` 动态追加子步骤并保持父节点分组顺序；
   - `plan_update` 更新子步骤为 `in_progress` 时自动将 `pending` 父步骤推进为 `in_progress`；
   - `plan_update` 在子步骤未全部完成时默认拒绝把父步骤直接标 `done`，传 `complete_children: true` 时一并收口；
   - 与 `rewind` 回退快照和 SSE `plan/updated` 事件完全一致；
   - 运行 `cargo test --bin codex-workspace-mcp -- plan_tools tool_wiring_invariants`，**0 失败、0 警告**。
2. **前端验证**：
   - 补充 `latestPlan` / `groupPlanSteps` 加权进度与两层分组的前端单测（如有现有测试目录则放一起）；
   - 运行 `npm --prefix frontend run typecheck`（0 错误）与 `npm --prefix frontend run build`（通过）。
3. **执行约束**：
   - 分支严格保持 `feat/context-as-code-v2`，**禁止碰 `master`**；
   - **不要与 SSH 任务并行编译同一个共享 `target/`**（错开执行或完成后再测），编译前先用 `df -h .` 确认磁盘可用 > 20 GB；
   - **不要终止或重启正在运行的 `grow-desktop` 进程**，完成后汇报改动文件与测试结果即可。
