# 麒麟 ARM64：QuickAgent 与 AI PPT 工具环境

此入口只准备开发、编译和 PPT 渲染依赖，不编译应用、不启动业务服务，也不访问数据库。覆盖 `codex-workspace-mcp`、`ai-ppt-server`、`ai-ppt-view` 和独立的 `pptx-editor-engine`。

本次按用户确认的功能分支对齐：后端 `codex/ppt-remote-svg`（基线 `5e683e3`），前端 `codex/ppt-host-agent`（基线 `7aba4f7`），不切换到 master。准备上下文时会校验分支，并将提交、工作区状态和后端依赖文件校验值写入镜像内的 `/opt/environment/project-sources.json`。

## 镜像与兼容范围

| 镜像 | 用户空间 | 用途 |
| --- | --- | --- |
| `quickagent-kylin-arm64-env:v1` | 麒麟 V10 SP3（Lance），ARM64，glibc 2.28 | Rust/WASM、Node、Go、Python、PPT/SVG、中文字体 |
| `quickagent-ppt-browser-arm64-env:v1` | 官方 Playwright 1.62.1 Noble，ARM64 | Chromium、HTML/PPT 浏览器渲染 |

照片目标机为 `aarch64`、麒麟 V10（Halberd）、内核 `4.19.90-89.43.v2401.ky10.aarch64`；用户于 2026-11-11 补充确认其 glibc 为 2.28，与本工具镜像基线一致。Lance 镜像不是 Halberd 的完整复刻，最终原生运行仍需在目标机验证。

工具镜像保留麒麟 glibc 2.28。Node 固定 22.22.1：[Node 22 官方平台要求](https://github.com/nodejs/node/blob/v22.x/BUILDING.md#platform-list)为 ARM64、内核至少 4.18、glibc 至少 2.28；官方二进制还需要 `GLIBCXX_3.4.25`。镜像内单独放置经校验的 C++ 运行库于 `/opt/cxx-runtime`，不替换系统 glibc 或 `/usr/lib64/libstdc++`。

现代浏览器所需的系统库放在第二个镜像里，不把这些二进制直接复制到麒麟运行。两个容器都共享宿主内核，本机 QEMU 检查不会模拟目标机的 4.19 内核。当前环境面向服务器/Web 版；Tauri 桌面包的 WebKitGTK 4.1 和 GUI 环境不在此入口中。

## 准备环境

构建机需要 Linux Docker Engine、Bash、Python 3.11 及以上版本，以及首次下载工具和依赖所需的网络。

```bash
cd /path/to/codex-workspace-mcp
bash scripts/kylin-arm64-env.sh build
```

上下文位于被 Git 忽略的 `target/kylin-arm64/environment-context/`，只包含环境定义、校验过的工具归档和 AI PPT 后端的 `requirements.txt`，不复制应用源码、配置、密钥或会话。工具下载版本与 SHA-256 记录在 `docker/kylin-arm64/toolchain-lock.json`；npm 使用锁文件。镜像内 `/opt/environment/python-packages.lock`、`rpm-packages.lock` 记录实际安装版本。

默认使用相邻的 `ai-ppt-server`、`ai-ppt-view` 和 `$HOME/project/pptx-editor-engine`，可覆盖：

```bash
export AI_PPT_SERVER_ROOT=/path/to/ai-ppt-server
export AI_PPT_VIEW_ROOT=/path/to/ai-ppt-view
export PPTX_ENGINE_ROOT=/path/to/pptx-editor-engine
```

基础镜像固定 ARM64 manifest 摘要，可通过 `KYLIN_BASE_IMAGE` 更换企业镜像。麒麟 yum 源改为厂商 HTTPS 地址并保留 RPM 签名校验。脚本优先在宿主解析厂商域名，必要时查询 DNS over HTTPS，仅为当前 Docker 构建增加地址映射；也可通过 `KYLIN_REPO_IP` 提供地址。它不会改宿主 DNS、`/etc/hosts` 或 Docker daemon 配置。

构建默认使用 host 网络以支持本机回环代理；代理作为 Docker 临时构建参数传入，不写入镜像 ENV。可设置 `KYLIN_BUILD_NETWORK=bridge`。Python 编译并行度通过 `KYLIN_BUILD_JOBS` 调整，默认 4。

## 使用与验证

```bash
# 无外网环境检查；不启动应用
bash scripts/kylin-arm64-env.sh check

# 进入麒麟编译环境
bash scripts/kylin-arm64-env.sh shell

# 进入 PPT 浏览器环境
bash scripts/kylin-arm64-env.sh browser-shell

# 执行工具命令
bash scripts/kylin-arm64-env.sh shell rustc --version
```

shell 将项目挂载到 `/work/agent`、`/work/ai-ppt-server`、`/work/ai-ppt-view`、`/work/pptx-engine`，允许读写。`node_modules` 与 Cargo 编译输出使用独立 ARM64 volume，避免误用宿主的 x86/Windows 原生模块。按项目自己的锁文件安装依赖；入口不会自动安装项目依赖或触发项目构建。

本次是客户交付环境，shell 默认注入 `PPT_CUSTOMER_EDITION=1`。这样从该入口进行后续前端构建时，只显示当前角色已授权的“创建 PPT”和“历史会话”；普通开发环境仍可显式用 `PPT_CUSTOMER_EDITION=0` 保持原菜单。此开关收敛导航，不替代后端的角色/API 授权。

`PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1` 避免前端 postinstall 在麒麟里重复下载不适配的浏览器；需要浏览器的操作放到 browser-shell 执行。镜像内 `/opt/ppt-tools/node_modules` 提供独立的 Canvas、Playwright、vite-node、jsdom 与 hashes 工具依赖。

浏览器镜像的 `PPT_DESIGN_BROWSER` 指向 Chromium 启动包装器；宿主入口根据 Docker daemon 的真实架构设置 `QUICKAGENT_ARM64_EMULATED`，仅仿真运行时禁用 GPU/zygote 子进程。原生 ARM64 保持正常的多进程方式。直接在 x86 上用 `docker run` 启动此浏览器镜像时，也应传入 `-e QUICKAGENT_ARM64_EMULATED=1`；QEMU 会合成 ARM `/proc/cpuinfo`，不能据此判断宿主架构。

检查会编译并执行小型 C/Rust/Go 样例，编译 WASM 并由 Node 调用导出函数，验证 Python PPTX 读写、Cairo/SVG 转 PNG、原生 Canvas、中文字体和模块加载。浏览器检查使用本地 HTML 并截图。小型工具探针不等于应用或业务链路验收。

`bwrap` 已安装；QEMU/容器内的用户命名空间不等同于原生麒麟上的命令沙箱，目标机需另行验证其内核策略。

## 修复 exec format error

x86_64 宿主未注册 ARM64 `binfmt_misc` 处理器时会出现此错误。环境入口先执行真实 ARM64 探针，失败时仅注册 ARM64 QEMU，再重试。注册方法遵循 [Docker 多平台构建说明](https://docs.docker.com/build/building/multi-platform/)，使用固定摘要的 `tonistiigi/binfmt` 特权容器。

```bash
bash scripts/setup-docker-arm64.sh

# 可选：以 root 安装开机恢复服务
sudo bash scripts/setup-docker-arm64.sh --persist
systemctl status quickagent-arm64-binfmt.service
```

开机服务在 Docker 和 systemd-binfmt 之后恢复 ARM64 注册，不启动应用；它只用已缓存的镜像，不在开机时访问网络。原生 ARM64 Docker 通过探针后无需注册 QEMU。

## 离线转移

```bash
docker save -o quickagent-ai-ppt-arm64-env.tar \
  quickagent-kylin-arm64-env:v1 quickagent-ppt-browser-arm64-env:v1
# ARM64 目标机：docker load -i quickagent-ai-ppt-arm64-env.tar
```

工具镜像不含应用发布包、数据库、项目 npm/Cargo 缓存或业务配置。环境检查不需要外网；后续项目依赖安装是否需要网络取决于各项目缓存。

## 客户模型配置

客户包的初始提供商、模型、默认路由和凭据全部为空，数据库也不预置模型。部署后由客户自行配置；启动和重启不要求已有模型或 DeepSeek key。联网搜索默认未启用，视觉模型跟随主模型，PPT 默认可使用客户后来登记的模型。导出脚本不读取开发机模型配置或凭据，也不覆盖本机开发配置：

```bash
# Python 需有 PyYAML；麒麟工具镜像内已提供。
python3 scripts/prepare-ppt-customer-profile.py \
  --source "$HOME/.config/grow" \
  --output target/kylin-arm64/customer-profile
```

输出目录必须尚不存在，避免覆盖客户后来填入的配置。YAML 配置文件使用 YAML 兼容的 JSON 形式保存。交付时只复制这个新目录作为 Agent 配置，并将 `GROW_CONFIG_DIR` 指向它。使用 DeepSeek 时才需要 `DEEPSEEK_API_KEY`；自部署 OpenAI 兼容服务只需地址和实际模型 ID，未启用鉴权时 key 留空。模型发现、聊天和图片探测都不会发送空的 Authorization 请求头。不要把开发机整个 `~/.config/grow`、凭据、会话或缓存打入客户包。

旧版客户配置升级时，只对带有出厂 `customer-policy.json` 且未迁移的目录处理一次：完全匹配出厂 DeepSeek 两模型的 PPT 允许列表改为不限制；完全匹配出厂的 DeepSeek 视觉路由改为跟随主模型。定制过的允许列表、视觉路由及其他设置保留；联网搜索选择也保留，但其缺少 key 不再阻塞模型配置保存。联网搜索本身仍需独立的有效 DeepSeek 凭据。

`customer-policy.json` 的提供商和模型列表为空，仅保留菜单路径和客户构建策略；`credentials.yaml` 只有空结构。AI PPT 的启动器会隔离旧 DSH profile 迁移。前后端端口和路由前缀不是默认 8899、`/prod-api` 时，还需按部署配置调整导出目录里的 `skill-packages.json` 插件地址。

## 客户离线部署包

完整构建入口为 `bash scripts/build-kylin-arm64-customer.sh`。先准备上述两个 SDK 镜像；当前构建驱动面向 Linux x86_64 开发机，使用本机 Rust 1.98.1 交叉编译并由麒麟 ARM64 容器内的 GCC 链接，验证 glibc 需求不超过 2.28。Go 以 ARMv8.0、CGO 关闭构建；前端和原生 Node 依赖在 ARM64 SDK 中安装，PPT 引擎的浏览器和 Node 两份 WASM 均重新生成。锁文件遗漏的平台可选依赖按锁定的精确版本补齐，并记录 npm 完整性值。

脚本先对当前三个项目和 PPT 引擎取最新工作区的隔离快照，不切换分支。当前功能已合入 master，构建不再要求旧功能分支；客户版在隔离源码中去掉素材库入口。快照包含未提交的当前代码，排除开发配置、凭据、SQL 备份、运行数据和宿主 node_modules。提交、分支、快照摘要及打包脚本摘要均写入 manifest。

部署由 Kylin glibc 2.28 应用容器、独立 Chromium 渲染容器、MySQL 8 和 Redis 7 组成。发布客户 Web 端口 8899，以及 QuickAgent 管理入口 `http://服务器地址:3001/agent/`（设置页 `/agent/settings`）。后者通过容器 3002 上的 `customer-gateway` 校验 Basic Auth，再转发到仅监听回环地址 3001 的 Agent；用户名 `admin`，密码始终为 `.env` 的 `CUSTOMER_ADMIN_PASSWORD`，不随 PPT 页面内的密码修改而变化。`QUICKAGENT_PORT`、`QUICKAGENT_BIND` 可调整宿主映射。健康检查同时覆盖 PPT、MCP 和网关认证后的设置 API。初始化器只接受空库，使用内置表结构生成新数据，再保留两个 PPT 菜单、一个管理员和非自用 `deepseek` 的 `deepseek-flash` / `deepseek-v4-pro`；补齐所选功能分支新增的插件 RPC 权限。客户密钥及随机密码在目标机初始化时填写或生成，构建包不包含开发凭据。

组装镜像和启动旧数据目录时都会清理联通品牌内置 PPTX、布局文件及目录索引；客户静态 Web 目录不携带示例 PDF/PPTX。设置窗口只在打开时加载配置，展开提供商和切换页签保留草稿；保存及图片探测期间暂停编辑，关闭后迟到的加载或保存响应不再写入下一次打开的窗口。

构建入口会在隔离 Compose 项目里执行真实登录、菜单/模型目录检查、插件认证、会话创建、中文 HTML→PPTX→PNG 渲染和浏览器页面验证。x86 QEMU 的 Redis 测试单独关闭持久化并覆盖受模拟地址影响的 COW 探针；交付的原生 ARM64 Compose 保留 Redis 原有检查。验证成功后才导出四个 ARM64 镜像，重新加载验证归档，并生成 SHA256SUMS、tar.gz 和外部校验文件。未持有客户有效 DeepSeek 密钥，因此验证不向模型 API 发起生成请求；目标物理 Halberd 服务器仍需现场验收。

产物默认在 `target/kylin-arm64/releases/`。可通过 `CUSTOMER_BUILD_WORK` 和 `CUSTOMER_RELEASE_DIR` 固定构建与交付目录，已有快照使用 `--resume`。部署目录自带中文说明及 `deploy.sh load/init/up/status/logs/stop`；目标需预装 ARM64 Docker Engine 24+ 和 Compose v2。
