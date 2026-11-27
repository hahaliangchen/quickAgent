# 项目备注

## 测试调查与构建隔离

- 诊断现有失败时，先读用户给出的日志和已有调用记录。有效结果可继续使用；缺少临时源码树不等于已有结果失效。需要补测时明确对应源码和测试，不为找回日志重跑。
- 本项目的只读命令沙箱中 `.git` 不可写，`git worktree add` 不可用。历史提交用 `git archive <commit>` 解到 git 忽略的目录。`/tmp` 每次命令独立；`target/` 是宿主持久目录，不会由沙箱在调用后自动回收。
- 历史调查使用自己唯一的目录，例如 `target/investigations/<run-id>/`，分别存放 `sources/<commit>/`、`builds/<commit>/` 和 `logs/`。每棵源码树设置独立的绝对路径 `CARGO_TARGET_DIR`，不要指向项目根的共享 `target/`，也不要让其他 agent 在同一源码树或构建目录并行编译。正常开发验证也与历史调查隔离。不要用全局 `cargo clean` 或删除别人的调查目录来修复缓存。
- 目录切换、归档解包和编译失败时立即停止该项检查；不要写 `cd ... && cargo clean ...; cargo test ...` 这种前一步失败仍会继续测试的命令。管道保留真实退出码（例如启用 `pipefail`），完整 stdout/stderr 一次保存到自己的日志。测试断言失败是调查结果，与编译失败、零匹配和执行中断分开记录。
- 每棵历史树首次运行前，记录提交、源码目录、构建目录，并核对测试列表及匹配名称；零匹配不能算通过。测试总数只能辅助核对，不能证明产物来源正确。不要靠 `touch` 或共享目录的 `cargo clean -p` 在不同源码之间切换；尤其不能用 `-not -path "*/target/*"` 筛选放在 `target/` 下的源码，它会排除整棵树。
- 每份调试构建约 4 GB，按提交编译会很快占满磁盘。每个提交的测试跑完、完整日志写入 `logs/` 后，立即删除自己的 `builds/<commit>/`，只保留 `sources/<commit>/`、日志和状态文件；之后需要复查时按源码重新编译。只有接下来马上要复用，或者有进程正在运行其中的程序时，才保留这份构建。每次开始编译前用 `df` 检查可用空间，低于 20 GB 时先停止并说明情况，不要等编译失败才发现。
- 正常构建前优先用 `sys.disk_space({})` 检查可用空间（可替代 `df -h`）。
- 保留原始测试日志和作废原因供后续查阅，不在调查结束时顺手清理其他调用仍可能需要的证据。构建目录不算证据，日志才是。判断预期行为以 `docs/agent-design-guide.md` 为准；看提交 diff 时区分删除的旧规则和新增的规则。

## 跨平台开发规范（Linux / Windows / macOS）

- 本项目面向 Linux、Windows 与 macOS 三端发布；开发与修改宿主库、内置插件、外部插件、前端路径逻辑及打包脚本时，严禁以本机 Ubuntu/Linux 环境为默认假设。
- **路径与 `\\?\` 前缀**：`std::fs::canonicalize` 产出的路径在比较（`strip_prefix`）、返回前端、传给外部命令（`explorer`、`tsc`、`git` 等）前，统一经过 `crate::tools::file_edit::strip_verbatim_prefix` / `display_path` 剥离 Windows `\\?\` 前缀。
- **分隔符与盘符归一**：返回给前端、写入 SQLite/JSON 协议或参与匹配的相对路径统一归一化为 POSIX `/`；输入校验同时兼容 `/` 与 `\`（含末尾分隔符检查）、Windows 盘符大小写（`C:/` 与 `c:\`）以及 `file:///C:/...` URI；正则或排除模式（如 Python `compileall -x`）需同时兼容 `/` 与 `\`。
- **后台子进程与无黑框**：Rust 后台启动任何子进程（`git`、`python`、`node`、插件本地服务等）时，在 `#[cfg(windows)]` 下必须设置 `creation_flags(0x08000000)`（`CREATE_NO_WINDOW`），避免 Windows 桌面端闪出控制台黑框；退出清理需兼顾 Windows Job Object / 进程树终止。
- **避免 Linux 专属 Shell 依赖**：产品代码与工具实现优先使用跨平台 Rust 原生能力（`std::fs`、`sys_info`、`http_probe`、`Command::new` 静态 argv），不依赖 `df`、`ss`、`ps`、`stat`、`pkill`、`xdg-open` 等单平台命令；涉及 OS 原生能力（打开文件夹、文件选择器、凭据存储、桌面自动化、Shell 提示）必须补齐 `windows` / `macos` / `unix` 三端实现。
- **平台特性与环境变量隔离**：Unix 专属 API（如 `std::os::unix::fs::PermissionsExt`）必须置于 `#[cfg(unix)]` 下并提供 `#[cfg(not(unix))]` 降级分支；用户目录/配置目录读取需同时兼容 `HOME` 与 Windows `USERPROFILE` / `APPDATA`。
- **跨平台单测覆盖**：涉及路径解析、URI 解码、工作区边界校验或平台分支的新增/修改代码，须补充覆盖 Windows 盘符、反斜杠与 `\\?\` 前缀的单元测试。
