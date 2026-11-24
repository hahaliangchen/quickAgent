# 内部日报助手（Daily Report）插件施工计划

> **适用仓库与分支**：`codex-workspace-mcp` 当前分支 `feat/context-as-code-v2`（**严禁触碰 `master`**）。
>
> **特殊隔离要求（必须严格遵守）**：
> - 本插件为团队内部办公管理系统（`192.168.100.145:8882`，基于 `report_backend` / `report_vue` 的 RuoYi-Vue-Plus）专用插件。
> - **源码随仓库提交到远程，但对外正式发布绝不打包、绝不启用！**
> - Rust 侧使用**非默认 Cargo feature** `internal-daily-report`（`[features]` 中 `default = []`）门控整个模块；`desktop/tauri.conf.json` 与所有客户/对外打包脚本（如 `docker/kylin-arm64/`）**严禁**开启该 feature 或打包 `src/server/builtin_plugins/daily_report/` 资源目录。
> - 前端不进左侧主图标栏，仅挂在 **工作台（SkillWorkbench）** 内；且仅当后端下发 `id === "daily-report"` 插件时才展示入口。
> - **不开启 feature 时，全仓编译与单测必须 100% 通过、0 warning。**

---

## 1. 对接的真实日报系统契约（已核验 `report_backend` / `report_vue`）

### 1.1 登录认证（纯用户名 + 密码，无图形验证码）
- 默认 API 基址：`http://192.168.100.145:8882/prod-api`（登录界面可自由编辑）。
- 固定客户端请求头：`clientid: e5cd7e4891bf95d1d19206ce24a7b32e`（支持配置覆盖）。
- `POST {base_url}/auth/login`：
  - 请求体 JSON（明文结构）：
    ```json
    {
      "tenantId": "000000",
      "username": "<username>",
      "password": "<password>",
      "clientId": "e5cd7e4891bf95d1d19206ce24a7b32e",
      "grantType": "password"
    }
    ```
  - **RuoYi-Vue-Plus 接口加密（默认开启）**：
    1. 随机生成 32 字节 ASCII 字母数字 AES-256 密钥 `aes_key`；
    2. 请求体 JSON 使用 `AES-256-ECB + PKCS7` 加密后做 Base64，作为 HTTP Body 发送（`Content-Type: application/json;charset=utf-8`）；
    3. 对 `Base64(aes_key)` 使用系统 RSA 公钥（PKCS1v15）加密后做 Base64，放入请求头 `encrypt-key`；
       - 默认 RSA 请求公钥（来自 `report_vue/.env.production`）：
         `MFwwDQYJKoZIhvcNAQEBBQADSwAwSAJBAKoR8mX0rGKLqzcWmOzbfj64K8ZIgOdHnzkXSOVOZbFu/TJhZ7rFAN+eaGkl3C4buccQd/EjEsj9ir7ijT7h96MCAwEAAQ==`
    4. 若响应头包含非空 `encrypt-key`，使用对应 RSA 响应私钥解出 Base64 AES Key，再用 `AES-256-ECB + PKCS7` 解密响应正文得到 JSON；
    5. 登录成功后解析 `data.access_token`，再调用 `GET {base_url}/system/user/getInfo`（带 `Authorization: Bearer <token>` 与 `clientid`）缓存 `userId`、`userName`、`nickName`。
  - **兜底认证模式**：登录弹窗同时提供「使用 Token / API Key 登录」切换项（直接保存 Bearer Token / API Key，跳过加密登录，方便密钥轮换时应急）。
  - **本地安全存储**：会话 Token、服务地址与用户信息存于 `~/.config/grow-daily-report/session.json`（或 `$DAILY_REPORT_DATA_DIR/session.json`），文件权限严格 `0600`；**绝不向前端或 AI 工具返回明文 `access_token` 或密码**，绝不写进任何业务代码仓库。

### 1.2 日报与项目接口（核验自 `WrDailyController.java` + `WrDailyBo.java` + `WrDailyProjects.java`）
所有业务请求携带 `Authorization: Bearer <access_token>` 与 `clientid`：
- **项目下拉列表**：`GET {base_url}/system/project/list?pageNum=1&pageSize=200`
- **历史日报列表**：`GET {base_url}/report/daily/list?pageNum=1&pageSize=20&reportYear=...&reportMonth=...&reportDate=...&userId=...`
- **日报详情**：`GET {base_url}/report/daily/{id}`
- **新增日报**：`POST {base_url}/report/daily`（JSON Body `WrDailyBo`）：
  ```json
  {
    "reportDate": "2026-11-24",
    "reportYear": "2026",
    "reportMonth": "11",
    "userName": "梁辰",
    "userId": "123",
    "problems": "",
    "remark": "",
    "overtimeHours": 0,
    "leaveHours": 0,
    "reimbursement": 0,
    "reimbursementReason": "",
    "projects": [
      {
        "projectId": 1001,
        "projectName": "雅砻江知识运营管理",
        "workContent": "1. 具体任务或功能：...\n2. 今日完成工作：..."
      }
    ]
  }
  ```
- **修改日报**：`PUT {base_url}/report/daily`（同上，附带主键 `id`）
- **删除日报**：`DELETE {base_url}/report/daily/{ids}`

> 注意：`projects` 是数组，天然支持一天填报多个项目的工作内容！如果当天（`reportDate`）当前用户已有日报记录，默认可选择「更新今日已有日报（`PUT`）」或「新增」。

---

## 2. 后端插件设计（`src/server/builtin_plugins/daily_report/`）

受 `#[cfg(feature = "internal-daily-report")]` 门控，模块拆分：
- `mod.rs` / `package.rs` / `plugin.json` / `SKILL.md`：
  - `id`: `"daily-report"`，`library`: `"daily_report"`，`title`: `"团队日报"`
  - 页面声明：`pages: [{ "id": "workbench", "title": "团队日报", "entry": "daily-report.workbench", "renderer": "react" }]`
- `crypto.rs`：实现 RuoYi-Vue-Plus `RSA PKCS1v15 + AES-256-ECB PKCS7` 登录加解密（复用现有纯 Rust `aes` / `rsa` 等依赖，附单元测试验证与 `report_vue/src/utils/crypto.ts` 完全互通）。
- `store.rs`：
  - 管理数据目录 `~/.config/grow-daily-report/`（`0700`）：
    - `session.json`（`0600`）：`base_url`、`client_id`、`access_token`、`user_id`、`user_name`、`nick_name`、`updated_at`（提供脱敏视图给前端）；
    - `guide.md`：**日报写作规范文档**（首次访问时若不存在自动创建空文件，供内置 `markdown` 插件与 AI 双向读写）；
    - `draft.json`：当前待确认的今日日报草稿（支持前端刷新不丢失）。
- `client.rs`：封装 `reqwest` 异步 HTTP 客户端调用登录、用户信息、项目列表、日报列表/详情/新增/修改/删除；遇 401 自动返回明确的 `AUTH_EXPIRED` 状态引导前端弹出登录框。
- `activity.rs`：实现**今日工作聚合**：只读查询 QuickAgent 主库 `codex_state.db` 中今天日期（本地时区 `YYYY-MM-DD`）的 `agent_tasks` 标题/摘要与 `agent_file_changes` 改动文件清单，同时可选扫描已打开项目今天的 `git log --since="00:00:00"` 提交记录，按项目分组返回精简事实摘要（零编造）。
- `routes.rs`（挂在 `src/server/ai_proxy.rs` 回环路由下，受 `#[cfg(feature = "internal-daily-report")]` 与同源浏览器校验保护，前缀 `/api/daily-report`）：
  - `GET /api/daily-report/status`：返回登录状态（脱敏：是否已登录、`base_url`、用户名/昵称）、`guide_dir`、`guide_exists`、`draft`。
  - `POST /api/daily-report/login`：接收 `{ base_url, username, password, client_id?, tenant_id? }` 或 `{ base_url, token, client_id? }`，登录并落 `0600` 会话文件。
  - `POST /api/daily-report/logout`：清除本地会话文件。
  - `GET /api/daily-report/projects`：代理公司 `/system/project/list`。
  - `GET /api/daily-report/reports`：代理公司 `/report/daily/list`（支持年份/月份/日期/项目筛选）。
  - `POST /api/daily-report/reports` / `PUT /api/daily-report/reports` / `DELETE /api/daily-report/reports/{id}`：代理提交/修改/删除日报（提交成功后自动清空本地草稿）。
  - `GET /api/daily-report/draft` / `PUT /api/daily-report/draft` / `DELETE /api/daily-report/draft`：读写本地草稿。
  - `GET /api/daily-report/guide` / `PUT /api/daily-report/guide`：读写 `guide.md` 文本（供快捷回退与状态同步；主编辑直接复用 `markdown` 插件）。
  - `GET /api/daily-report/today-activity`：返回今天跨项目会话与 Git 提交汇总，前端页面也可一键点「拉取今日工作记录」。

### 暴露 6 个 AI 工具（`library: "daily_report"`）

| 工具名 | 权限与审批 | 作用 |
| :--- | :--- | :--- |
| `daily_report__read_guide` | `ReadOnly` | 读取 `guide.md` 写作规范与目录路径（若为空明确告知 `empty: true`） |
| `daily_report__write_guide` | `WorkspaceWrite` | 写入或更新 `guide.md`（方便用户用 `db` 插件分析完日报数据库后直接让 AI 总结沉淀写作规范） |
| `daily_report__today_activity` | `ReadOnly` | 聚合今天（可传 `date: "YYYY-MM-DD"`）你在 QuickAgent 各项目里的任务记录、文件改动与 Git 提交 |
| `daily_report__list` | `ReadOnly` | 调用日报系统获取可选项目列表与近期已提交日报（未登录时返回清晰提示引导用户去工作台登录） |
| `daily_report__prepare_draft` | `ReadOnly`（写插件私有草稿） | 保存 AI 按 `guide.md` 生成的结构化日报草稿（支持多项目 `projects[]`），并向会话发射 `daily_report/draft_ready` 事件——**触发前端自动跳转到工作台的「团队日报」页并回填表单** |
| `daily_report__submit` | **强制 `once_only` 人工审批**（参照 `ssh__add_server`） | 真正向公司日报系统调用 `POST` 或 `PUT /report/daily`。审批卡片清晰展示日期、涉及项目名、工作内容预览与动作类型（新增/更新），必须用户点击「允许这一次」才发送 |

---

## 3. 前端工作台页面设计（`frontend/src/builtin_plugins/daily-report/`）

- **挂载位置**：不进左侧 `CodexDock` 主栏，注册在 `frontend/src/builtin_plugins/index.ts`（`'daily-report'`），在 **工作台（`SkillWorkbench`）** 内作为独立插件页展示；当后端未启用 `internal-daily-report` feature 时，工作台自动不显示该入口。
- **跨会话自动跳转联动（复刻 SSH 实时跟随机制）**：
  - 在 `App.tsx` 监听会话事件 `daily_report/draft_ready`：当用户在任意项目会话说“把今天干的给我写个日报”且 AI 调完 `daily_report__prepare_draft` 后，自动打开工作台并切换到「团队日报」页，自动载入刚刚生成的草稿（同时保留右侧或快捷返回原会话入口，用户一眼确认无误即可点「提交到日报系统」）。
- **页面内部结构（适配 Light Paper / Dark Ink 双主题）**：
  1. **未登录态：简洁的登录认证界面**（参考截图里的居中卡片风格）：
     - 标题「登录团队日报系统」
     - 表单项：服务地址（默认 `http://192.168.100.145:8882/prod-api`，可折叠高级项含 `clientId`）、**用户名**、**密码**、登录按钮；
     - 顶部轻切换：「账号密码登录」 / 「Token 登录」；
     - 登录失败清晰展示后端报错（密码绝不回显或存 localStorage）。
  2. **已登录态：顶栏 + 两个清晰子 Tab**：
     - 顶栏：当前登录人昵称（如 `梁辰`）、服务地址胶囊、「切换账号 / 退出」、以及两个主 Tab：**「日报填报」** 与 **「写作规范 (Markdown)」**。
     - **子 Tab 1：日报填报**
       - **左侧/上方：今日日报表单**
         - 顶部操作条：日期选择（默认今天）、「从今日会话生成草稿（唤起 AI）」、若存在 AI 草稿显示蓝色提示条「已载入 AI 生成的日报草稿」；
         - **多项目卡片列表（`projects[]`）**：每条含「项目名称」搜索下拉（来自 `/system/project/list`）+「工作内容」多行编辑框 + 删除，支持 `+ 添加项目`；
         - 公共字段：遇到的问题、加班时长、请假时长、报销费用、备注；
         - 底部提交按钮：**「提交日报」**（若今日已提交过则自动提示并切换为「更新今日日报」）。
       - **右侧/下方：历史日报列表**
         - 筛选条（年份 / 日期 / 项目名 / 刷新），卡片或表格展示历史日报，点击某条可「查看详情」或「载入到表单修改」。
     - **子 Tab 2：写作规范（Markdown）——直接复用现有 `markdown` 插件！**
       - 顶部一行简短提示：`AI 生成日报前会自动读取这份规范；你可以直接编辑，或用数据库插件分析历史日报后让 AI 帮你总结写入。`
       - 主体直接挂载 `<PluginPageHost owner="markdown" page="studio" workspaceRoot={guideDir} previewRequest={{ path: 'guide.md', ... }} theme={theme} />`（若 `markdown` 插件被用户手动禁用，则平滑降级为内置双栏纯文本编辑 + Markdown 预览并提示启用 Markdown 插件）。

---

## 4. 验收标准与自测清单

1. **双编译模式零回归（最关键！）**：
   - 默认模式（**不带** feature）：`cargo test` 与 `cargo check` 必须 0 warning 通过，且编译产物中**不包含** `daily-report` 插件、`daily_report__*` 工具与 `/api/daily-report/*` 路由（专门写 1 个单测断言默认编译下 `builtin_plugins::packages()` 不含 `daily-report`）。
   - 内部模式（**带** `--features internal-daily-report`）：
     - `cargo test --features internal-daily-report daily_report` 全通过，覆盖：
       - `crypto.rs` 与 RuoYi `RSA + AES-256-ECB PKCS7` 加解密往返验证；
       - `store.rs` `0600` 权限、密码/Token 永不泄露到状态接口、`guide.md` 自动初始化为空并支持读写；
       - `client.rs` 对接本地 mock HTTP 服务验证登录、拉项目列表、新增/修改日报 JSON 负载与 401 过期处理；
       - `daily_report__submit` 强制 `once_only` 审批策略断言；
       - `read_tool_contracts` 对 `daily_report` 返回 6 个工具的完整 TS 签名与 `inputSchema`。
2. **前端与 Playwright 验收**：
   - `npm --prefix frontend run typecheck && npm --prefix frontend run build` 0 错误；
   - 新增轻量 Playwright 验收测试（mock 公司日报 HTTP 接口），验证：
     - 未登录显示登录卡片 → 输入账号密码登录成功进入日报页；
     - AI 触发 `daily_report/draft_ready` 后自动跳到工作台日报页并填好表单 → 点击提交成功调用 `POST /report/daily`；
     - 切换到「写作规范 (Markdown)」Tab 正确挂载并编辑 `guide.md`；
     - Light / Dark 双主题无控制台报错。
3. **打包隔离复核**：
   - 确认 `desktop/tauri.conf.json`、`docker/kylin-arm64/` 等对外发布配置中无任何 `daily_report` 引用。
