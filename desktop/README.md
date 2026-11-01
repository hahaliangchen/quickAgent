# Grow 桌面端

桌面端复用现有网页和 Rust Agent。窗口启动独立的本地 Agent；关闭窗口会停止该进程。网页服务原有启动方式不变。桌面版默认在系统应用数据目录下保存自身会话，项目仍可通过界面选择。

## 开发运行

安装 Rust、Node.js 及 [Tauri 2 的系统依赖](https://v2.tauri.app/zh-cn/start/prerequisites/)。在仓库根目录运行：

```sh
npm --prefix frontend ci
node desktop/prepare.mjs
cargo run --manifest-path desktop/Cargo.toml
```

Windows 需要 MSVC 构建工具和 WebView2；Ubuntu 需要 WebKitGTK 及 Tauri 文档列出的开发包。两个平台都需分别构建和测试。

## 打包

安装 Tauri CLI 2，先准备 release 版本后端，然后在 `desktop` 目录执行：

```sh
node desktop/prepare.mjs --release
cd desktop
cargo tauri build
```

构建产物位于 `desktop/target/release/bundle/`。应在目标操作系统上分别构建；应用需要本地 WebView 运行环境。本桌面外壳不为 Agent 命令提供操作系统沙箱。
