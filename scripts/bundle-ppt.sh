#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PPT_SRC="${1:-/home/goodtime/project/pptx-editor-engine}"
OUT_DIR="$REPO_ROOT/bundled_plugins/ppt"
FONT_BUILD_ROOT="$REPO_ROOT/target/ppt-font-server-build"

if [[ ! -f "$PPT_SRC/package.json" || ! -f "$PPT_SRC/rust-backend/Cargo.toml" ]]; then
  echo "Invalid pptx-editor-engine source directory: $PPT_SRC" >&2
  exit 1
fi

echo "==> 1/4 Building pptx-editor-engine frontend bundle..."
if [[ ! -f "$PPT_SRC/rust-engine/pkg/ppt_engine_bg.wasm" ]]; then
  (cd "$PPT_SRC" && npm run build:wasm)
fi
(cd "$PPT_SRC" && npx rspack build)

echo "==> 2/4 Assembling bundled_plugins/ppt/page/..."
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR/page" "$OUT_DIR/bin"
find "$PPT_SRC/dist" -maxdepth 1 -type f ! -name '*.map' -exec cp {} "$OUT_DIR/page/" \;

python3 - "$OUT_DIR/page/index.html" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
html = path.read_text(encoding="utf-8")
boot_head = """<style id="ppt-boot-style">
  html, body { margin: 0; height: 100%; background: #f5f5f7; overflow: hidden; }
  html:not([data-ppt-ready="1"]) .app-container { opacity: 0; visibility: hidden; pointer-events: none; }
  html[data-ppt-ready="1"] .app-container { opacity: 1; visibility: visible; transition: opacity 220ms ease; }
  #ppt-boot-loader {
    position: fixed; inset: 0; z-index: 99999;
    display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 14px;
    background: #f5f5f7;
    color: #1d1d1f;
    font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", system-ui, sans-serif;
    transition: opacity 220ms ease, visibility 220ms ease;
  }
  #ppt-boot-loader.is-done { opacity: 0; visibility: hidden; pointer-events: none; }
  .ppt-boot-badge {
    width: 48px; height: 48px; border-radius: 14px;
    display: flex; align-items: center; justify-content: center;
    background: #ffffff;
    box-shadow: 0 6px 20px rgba(15, 23, 42, 0.08), inset 0 0 0 1px rgba(60, 60, 67, 0.1);
    position: relative;
  }
  .ppt-boot-ring {
    width: 24px; height: 24px; border-radius: 50%;
    border: 2.5px solid rgba(16, 185, 129, 0.18);
    border-top-color: #10b981;
    animation: ppt-boot-spin 0.78s linear infinite;
  }
  .ppt-boot-title { font-size: 14px; font-weight: 600; letter-spacing: -0.01em; color: #1d1d1f; }
  .ppt-boot-sub { font-size: 12px; color: #6e6e73; }
  @keyframes ppt-boot-spin { to { transform: rotate(360deg); } }
</style>
<script>(function(){
  const origFetch = window.fetch.bind(window);
  const localApiBase = new URL("../../local-api/", window.location.href).toString();
  window.fetch = function(input, init) {
    const raw = typeof input === "string" ? input : (input instanceof Request ? input.url : String(input));
    if (raw.startsWith("http://127.0.0.1:8080/")) {
      const rewritten = localApiBase + raw.slice("http://127.0.0.1:8080/".length);
      return origFetch(input instanceof Request ? new Request(rewritten, input) : rewritten, init);
    }
    return origFetch(input, init);
  };
  let revealed = false;
  function reveal() {
    if (revealed) return;
    revealed = true;
    requestAnimationFrame(() => requestAnimationFrame(() => {
      document.documentElement.setAttribute("data-ppt-ready", "1");
      const el = document.getElementById("ppt-boot-loader");
      if (el) {
        el.classList.add("is-done");
        setTimeout(() => el.remove(), 260);
      }
    }));
  }
  function hasAppStyles() {
    return Array.from(document.head.querySelectorAll("style")).some(s => s.id !== "ppt-boot-style");
  }
  if (hasAppStyles()) {
    reveal();
  } else {
    const observer = new MutationObserver(() => {
      if (hasAppStyles()) { observer.disconnect(); reveal(); }
    });
    observer.observe(document.head, { childList: true });
    setTimeout(() => { observer.disconnect(); reveal(); }, 6000);
  }
})();</script>"""
boot_body = """<div id="ppt-boot-loader" role="status" aria-live="polite">
  <div class="ppt-boot-badge"><span class="ppt-boot-ring" aria-hidden="true"></span></div>
  <div class="ppt-boot-title">PPT 编辑器</div>
  <div class="ppt-boot-sub">正在加载 Rust WASM 渲染引擎…</div>
</div>"""
marker = '<script src="index.js"'
if marker not in html:
    raise SystemExit("Expected index.js script tag in dist/index.html")
html = html.replace(marker, boot_head + "\n" + marker, 1)
if "<body>" not in html:
    raise SystemExit("Expected <body> tag in dist/index.html")
html = html.replace("<body>", "<body>\n" + boot_body, 1)
path.write_text(html, encoding="utf-8")
PY

echo "==> 3/4 Building bundled ppt-font-server (Rust fontdb)..."
rm -rf "$FONT_BUILD_ROOT"
mkdir -p "$FONT_BUILD_ROOT/src"
cp "$PPT_SRC/rust-backend/Cargo.toml" "$FONT_BUILD_ROOT/Cargo.toml"
cp "$PPT_SRC/rust-backend/Cargo.lock" "$FONT_BUILD_ROOT/Cargo.lock"
python3 - "$PPT_SRC/rust-backend/src/main.rs" "$FONT_BUILD_ROOT/src/main.rs" <<'PY'
import pathlib, sys
src = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
old = 'let listener = tokio::net::TcpListener::bind("127.0.0.1:8080")'
new = 'let bind_addr = std::env::var("BIND").unwrap_or_else(|_| format!("127.0.0.1:{}", std::env::var("PORT").unwrap_or_else(|_| "8080".to_string())));\n    let listener = tokio::net::TcpListener::bind(&bind_addr)'
if old not in src:
    raise SystemExit("Unexpected bind statement in rust-backend/src/main.rs")
src = src.replace(old, new, 1)
pathlib.Path(sys.argv[2]).write_text(src, encoding="utf-8")
PY

CARGO_TARGET_DIR="$FONT_BUILD_ROOT/target" cargo build --release --manifest-path "$FONT_BUILD_ROOT/Cargo.toml" -j 2
cp "$FONT_BUILD_ROOT/target/release/rust-backend" "$OUT_DIR/bin/ppt-font-server"
chmod +x "$OUT_DIR/bin/ppt-font-server"
rm -rf "$FONT_BUILD_ROOT"

echo "==> 4/4 Writing plugin.json and SKILL.md..."
cat > "$OUT_DIR/plugin.json" <<'JSON'
{
  "version": 1,
  "id": "ppt",
  "library": "ppt",
  "title": "PPT 编辑器",
  "enabled_by_default": true,
  "skill": "SKILL.md",
  "pages": [
    {
      "id": "studio",
      "title": "PPT 编辑器",
      "entry": "page/index.html",
      "renderer": "html"
    }
  ],
  "local_service": {
    "program": "bin/ppt-font-server"
  }
}
JSON

cat > "$OUT_DIR/SKILL.md" <<'MD'
---
name: ppt
description: 本地 Rust WASM PPTX 演示文稿渲染与编辑工作台模块
---

# PPT 编辑器模块

本模块随 QuickAgent 模块化打包自动启用，提供基于 Rust WASM（`cosmic-text`）与 HTML5 Canvas 的本地 `.pptx` 渲染、编辑、模板预览与导出工作台。

- 点击左侧导航栏的 **PPT 编辑器** 图标即可直接打开工作台；
- 支持加载示例文稿、打开/拖入本地 `.pptx` 文件、编辑页面元素与排版、以及导出修改后的 `.pptx` 文件；
- 系统字体由随包附带的 `ppt-font-server` 本地助手按需自动提供，无需手工启动额外端口。
MD

echo "Done! Bundled PPT module size:"
du -sh "$OUT_DIR" "$OUT_DIR/page" "$OUT_DIR/bin"
