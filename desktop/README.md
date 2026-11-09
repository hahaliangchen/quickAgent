# Grow 桌面端

桌面端复用现有网页和 Rust Agent。窗口启动独立的本地 Agent；关闭窗口会停止该进程。网页服务原有启动方式不变。桌面版默认在系统应用数据目录下保存自身会话，项目仍可通过界面选择。

「创建项目」里点“添加”会打开系统自带的文件夹窗口（Tauri 对话框插件）。页面来自本地 Agent 的 `http://127.0.0.1`，属于远程来源，所以 `capabilities/agent-page.json` 只授予它 `dialog:allow-open` 这一个命令；网页版没有系统窗口可用，改为输入文件夹路径。

## 开发运行

安装 Rust、Node.js 及 [Tauri 2 的系统依赖](https://v2.tauri.app/zh-cn/start/prerequisites/)。在仓库根目录运行：

```sh
npm --prefix frontend ci
node desktop/prepare.mjs
cargo run --manifest-path desktop/Cargo.toml
```

Windows 需要 MSVC 构建工具和 WebView2；Ubuntu 需要 WebKitGTK 及 Tauri 文档列出的开发包。两个平台都需分别构建和测试。

## 命令运行环境

Windows 代码会话使用 PowerShell 7 及以上版本，不回退到 Windows PowerShell 5.1。可在“设置 → 通用 → 命令运行环境”指定 `pwsh.exe`；查找顺序为手动设置、`GROW_PWSH_PATH`、`%ProgramFiles%/PowerShell/7/pwsh.exe`、PATH 中的 `pwsh.exe`，最后尝试 `%LOCALAPPDATA%/Microsoft/WindowsApps/pwsh.exe`。Store 安装完成后可直接重新检测，无需重启 Grow。设置中显示实际使用的路径、版本和缓存检测时间。

仅代码会话会主动检查环境并显示缺失引导；普通聊天和插件不会弹出安装提示。用户点击“一键安装”后，才运行界面显示的固定 winget 命令，从 Microsoft Store 安装 PowerShell（产品 ID `9MZ1SNWT0N5D`）；也可打开系统 Store，或指定已有安装。安装进度可查看和取消，结束后重新检测。

Linux 保留 Bash，可手动设置路径或使用 `GROW_BASH_PATH`，默认查找 `/bin/bash` 和 PATH。Node 路径复用项目进程工具的解析方式。

桌面构建会准备固定版本并校验 SHA-256 的 rg，与后端一起打包。离线构建可将 `GROW_RG_ARCHIVE` 指向对应目标平台的官方 ripgrep 15.1.0 发布压缩包；构建仍会核对固定校验值。发行资源包含 ripgrep 许可证，以及 PowerShell 命令转义实现对应的 `DSH-LICENSE.txt` MIT 声明。

## 打包

安装 Tauri CLI 2，先准备 release 版本后端，然后在 `desktop` 目录执行：

```sh
node desktop/prepare.mjs --release
cd desktop
cargo tauri build
```

构建产物位于 `desktop/target/release/bundle/`。应在目标操作系统上分别构建；应用需要本地 WebView 运行环境。本桌面外壳不为 Agent 命令提供操作系统沙箱。
