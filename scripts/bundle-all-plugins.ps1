<#
.SYNOPSIS
  Build and bundle all 6 external editor/workbench plugins from the 4 sibling
  repositories (docx-editer-engine, excel-editor-engine, pdf-editor-engine,
  pptx-editor-engine) into `bundled_plugins/` on Windows.
#>
param(
  [string]$ProjectsRoot = 'D:\enterpriseProject',
  [string[]]$Only = @('word','markdown','excel','db','pdf','ppt'),
  [switch]$KeepExternalTargets
)

$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $PSScriptRoot
$BundledRoot = Join-Path $RepoRoot 'bundled_plugins'
New-Item -ItemType Directory -Force -Path $BundledRoot | Out-Null

# Prefer Node 22.14+ / 24 via fnm when available (Vite 7 requires Node >= 22.12).
if (Get-Command fnm -ErrorAction SilentlyContinue) {
  $fnmVersions = (& fnm list 2>$null) -join "`n"
  if ($fnmVersions -match 'v24\.21\.0') {
    & fnm env --shell power-shell | Out-String | Invoke-Expression
    & fnm use v24.21.0 | Out-Null
  } elseif ($fnmVersions -match 'v22\.14\.0') {
    & fnm env --shell power-shell | Out-String | Invoke-Expression
    & fnm use v22.14.0 | Out-Null
  }
}

function Should-Build([string[]]$Ids) {
  $selected = @($Only | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() })
  foreach ($id in $Ids) {
    if ($selected -contains $id) { return $true }
  }
  return $false
}

function Invoke-Checked {
  param([string]$Label, [string]$WorkDir, [scriptblock]$Action)
  Write-Host "`n==> $Label" -ForegroundColor Cyan
  Push-Location $WorkDir
  try {
    & $Action
    if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) {
      throw "$Label failed with exit code $LASTEXITCODE"
    }
  } finally {
    Pop-Location
  }
}

function Reset-Dir([string]$Path) {
  if (Test-Path -LiteralPath $Path) {
    cmd /c rmdir /s /q "`"$Path`"" 2>$null
    if (Test-Path -LiteralPath $Path) { Remove-Item -LiteralPath $Path -Recurse -Force }
  }
  New-Item -ItemType Directory -Force -Path $Path | Out-Null
}

function Copy-DirContents([string]$Src, [string]$Dst, [string[]]$ExcludeExts = @()) {
  New-Item -ItemType Directory -Force -Path $Dst | Out-Null
  Get-ChildItem -LiteralPath $Src -Force | ForEach-Object {
    if (-not $_.PSIsContainer -and ($ExcludeExts -contains $_.Extension.ToLowerInvariant())) {
      return
    }
    Copy-Item -LiteralPath $_.FullName -Destination $Dst -Recurse -Force
  }
}

# =========================================================================
# 1 & 2. docx-editer-engine -> bundled_plugins/word & bundled_plugins/markdown
# =========================================================================
if (Should-Build @('word','markdown')) {
$DocxRepo = Join-Path $ProjectsRoot 'docx-editer-engine'
if (-not (Test-Path (Join-Path $DocxRepo 'node_modules'))) {
  Invoke-Checked 'docx-editer-engine: npm ci' $DocxRepo { npm ci --no-audit --no-fund }
}
Invoke-Checked 'docx-editer-engine: build word & markdown frontends' $DocxRepo {
  npm run build:word
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  npm run build:md
}
Invoke-Checked 'docx-editer-engine: build rust-docx release' $DocxRepo {
  cargo build --release --locked --manifest-path rust-docx/Cargo.toml --bin rust-docx
}

# Assemble word
$WordOut = Join-Path $BundledRoot 'word'
Reset-Dir $WordOut
New-Item -ItemType Directory -Force -Path (Join-Path $WordOut 'page'), (Join-Path $WordOut 'bin') | Out-Null
Copy-DirContents (Join-Path $DocxRepo 'packages\word-editor\dist') (Join-Path $WordOut 'page')
Copy-Item -Force (Join-Path $DocxRepo 'rust-docx\target\release\rust-docx.exe') (Join-Path $WordOut 'bin\word-server.exe')
Copy-Item -Force (Join-Path $DocxRepo 'scripts\quickagent\SKILL.md') (Join-Path $WordOut 'SKILL.md')
# Generate word plugin.json using docx repo's manifest schema (with Windows .exe probe)
Invoke-Checked 'word: validate binary & generate plugin.json' $DocxRepo {
  $manifestScript = Get-Content -Raw 'scripts\quickagent\manifest.mjs'
  $patched = $manifestScript.Replace("const binary = join(root, program)", "const binary = join(root, process.platform === 'win32' ? program + '.exe' : program)")
  $tmpScript = Join-Path $env:TEMP "word-manifest-$PID.mjs"
  Set-Content -Path $tmpScript -Value $patched -Encoding UTF8
  try {
    node $tmpScript $WordOut
  } finally {
    Remove-Item -Force $tmpScript -ErrorAction SilentlyContinue
  }
}

# Assemble markdown
$MdOut = Join-Path $BundledRoot 'markdown'
Reset-Dir $MdOut
New-Item -ItemType Directory -Force -Path (Join-Path $MdOut 'page') | Out-Null
Copy-DirContents (Join-Path $DocxRepo 'packages\markdown-editor\dist') (Join-Path $MdOut 'page')
Copy-Item -Force (Join-Path $DocxRepo 'scripts\quickagent\markdown-SKILL.md') (Join-Path $MdOut 'SKILL.md')
Invoke-Checked 'markdown: validate & generate plugin.json' $DocxRepo {
  node 'scripts\quickagent\markdown-manifest.mjs' $MdOut
}

if (-not $KeepExternalTargets) {
  $t = Join-Path $DocxRepo 'rust-docx\target'
  if (Test-Path $t) { cmd /c rmdir /s /q "`"$t`"" 2>$null }
}
}

# =========================================================================
# 3 & 4. excel-editor-engine -> bundled_plugins/excel & bundled_plugins/db
# =========================================================================
if (Should-Build @('excel','db')) {
$ExcelRepo = Join-Path $ProjectsRoot 'excel-editor-engine'
if (-not (Test-Path (Join-Path $ExcelRepo 'node_modules'))) {
  Invoke-Checked 'excel-editor-engine: npm ci' $ExcelRepo { npm ci --no-audit --no-fund }
}
Invoke-Checked 'excel-editor-engine: build excel-studio & db-studio frontends' $ExcelRepo {
  npm run build
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  npm run build --workspace db-studio
}
Invoke-Checked 'excel-editor-engine: build rust-excel & rust-db release' $ExcelRepo {
  cargo build --release --locked --manifest-path rust-excel/Cargo.toml --bin excel-server
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  cargo build --release --locked --manifest-path rust-db/Cargo.toml --bin db-server
}

# Assemble excel
$ExcelOut = Join-Path $BundledRoot 'excel'
Reset-Dir $ExcelOut
New-Item -ItemType Directory -Force -Path (Join-Path $ExcelOut 'page'), (Join-Path $ExcelOut 'bin') | Out-Null
Copy-DirContents (Join-Path $ExcelRepo 'packages\excel-studio\dist') (Join-Path $ExcelOut 'page')
Copy-Item -Force (Join-Path $ExcelRepo 'rust-excel\target\release\excel-server.exe') (Join-Path $ExcelOut 'bin\excel-server.exe')
Copy-Item -Force (Join-Path $ExcelRepo 'scripts\quickagent\SKILL.md') (Join-Path $ExcelOut 'SKILL.md')
Invoke-Checked 'excel: generate plugin.json' $ExcelRepo {
  node 'scripts\quickagent\manifest.mjs' (Join-Path $ExcelOut 'plugin.json')
}

# Assemble db
$DbOut = Join-Path $BundledRoot 'db'
Reset-Dir $DbOut
New-Item -ItemType Directory -Force -Path (Join-Path $DbOut 'page'), (Join-Path $DbOut 'bin') | Out-Null
Copy-DirContents (Join-Path $ExcelRepo 'packages\db-studio\dist') (Join-Path $DbOut 'page')
Copy-Item -Force (Join-Path $ExcelRepo 'rust-db\target\release\db-server.exe') (Join-Path $DbOut 'bin\db-server.exe')
Copy-Item -Force (Join-Path $ExcelRepo 'scripts\quickagent-db\SKILL.md') (Join-Path $DbOut 'SKILL.md')
Invoke-Checked 'db: generate plugin.json' $ExcelRepo {
  node 'scripts\quickagent-db\manifest.mjs' (Join-Path $DbOut 'plugin.json')
}

if (-not $KeepExternalTargets) {
  foreach ($sub in @('rust-excel\target', 'rust-db\target')) {
    $t = Join-Path $ExcelRepo $sub
    if (Test-Path $t) { cmd /c rmdir /s /q "`"$t`"" 2>$null }
  }
}
}

# =========================================================================
# 5. pdf-editor-engine -> bundled_plugins/pdf
# =========================================================================
if (Should-Build @('pdf')) {
$PdfRepo = Join-Path $ProjectsRoot 'pdf-editor-engine'
if (-not (Test-Path (Join-Path $PdfRepo 'node_modules'))) {
  Invoke-Checked 'pdf-editor-engine: npm ci' $PdfRepo { npm ci --no-audit --no-fund }
}
if (-not (Test-Path (Join-Path $PdfRepo 'node_modules\@rollup\rollup-win32-x64-msvc'))) {
  $rollupVer = (Get-Content -Raw (Join-Path $PdfRepo 'node_modules\rollup\package.json') | ConvertFrom-Json).version
  Invoke-Checked "pdf-editor-engine: install Windows Rollup native addon ($rollupVer)" $PdfRepo {
    npm install --no-save --no-audit --no-fund "@rollup/rollup-win32-x64-msvc@$rollupVer"
  }
}
Invoke-Checked 'pdf-editor-engine: build pdf-studio frontend' $PdfRepo {
  npm run build -w pdf-studio
}
Invoke-Checked 'pdf-editor-engine: build rust-pdf release' $PdfRepo {
  cargo build --locked --release --manifest-path rust-pdf/Cargo.toml
}

$PdfOut = Join-Path $BundledRoot 'pdf'
Reset-Dir $PdfOut
New-Item -ItemType Directory -Force -Path (Join-Path $PdfOut 'page'), (Join-Path $PdfOut 'bin') | Out-Null
Copy-DirContents (Join-Path $PdfRepo 'packages\pdf-studio\dist') (Join-Path $PdfOut 'page')
Copy-Item -Force (Join-Path $PdfRepo 'rust-pdf\target\release\pdf-server.exe') (Join-Path $PdfOut 'bin\pdf-server.exe')
Copy-Item -Force (Join-Path $PdfRepo 'scripts\quickagent\SKILL.md') (Join-Path $PdfOut 'SKILL.md')
Invoke-Checked 'pdf: generate plugin.json' $PdfRepo {
  node 'scripts\quickagent\manifest.mjs' (Join-Path $PdfOut 'plugin.json')
}

if (-not $KeepExternalTargets) {
  $t = Join-Path $PdfRepo 'rust-pdf\target'
  if (Test-Path $t) { cmd /c rmdir /s /q "`"$t`"" 2>$null }
}
}

# =========================================================================
# 6. pptx-editor-engine -> bundled_plugins/ppt
# =========================================================================
if (Should-Build @('ppt')) {
$PptRepo = Join-Path $ProjectsRoot 'pptx-editor-engine'
if (-not (Test-Path (Join-Path $PptRepo 'node_modules'))) {
  Invoke-Checked 'pptx-editor-engine: npm ci' $PptRepo { npm ci --no-audit --no-fund }
}
Invoke-Checked 'pptx-editor-engine: build WASM & Rspack bundle' $PptRepo {
  wasm-pack build rust-engine --target bundler --out-dir pkg --no-opt
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  npx rspack build
}

$PptOut = Join-Path $BundledRoot 'ppt'
Reset-Dir $PptOut
New-Item -ItemType Directory -Force -Path (Join-Path $PptOut 'page'), (Join-Path $PptOut 'bin') | Out-Null
Copy-DirContents (Join-Path $PptRepo 'dist') (Join-Path $PptOut 'page') -ExcludeExts @('.map')

# Inject QuickAgent local-api fetch rewrite & boot loader into ppt index.html (matches scripts/bundle-ppt.sh)
$PptIndex = Join-Path $PptOut 'page\index.html'
@'
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
'@ | python - $PptIndex
if ($LASTEXITCODE -ne 0) { throw 'Failed to patch PPT index.html' }

# Build patched ppt-font-server.exe in temporary dir
$FontBuild = Join-Path $RepoRoot 'target\ppt-font-server-build'
Reset-Dir $FontBuild
New-Item -ItemType Directory -Force -Path (Join-Path $FontBuild 'src') | Out-Null
Copy-Item -Force (Join-Path $PptRepo 'rust-backend\Cargo.toml') (Join-Path $FontBuild 'Cargo.toml')
Copy-Item -Force (Join-Path $PptRepo 'rust-backend\Cargo.lock') (Join-Path $FontBuild 'Cargo.lock')
$fontSrc = Get-Content -Raw (Join-Path $PptRepo 'rust-backend\src\main.rs')
$oldBind = 'let listener = tokio::net::TcpListener::bind("127.0.0.1:8080")'
$newBind = 'let bind_addr = std::env::var("BIND").unwrap_or_else(|_| format!("127.0.0.1:{}", std::env::var("PORT").unwrap_or_else(|_| "8080".to_string())));' + "`n    " + 'let listener = tokio::net::TcpListener::bind(&bind_addr)'
if (-not $fontSrc.Contains($oldBind)) { throw 'Unexpected bind statement in pptx-editor-engine/rust-backend/src/main.rs' }
Set-Content -Path (Join-Path $FontBuild 'src\main.rs') -Value $fontSrc.Replace($oldBind, $newBind) -Encoding UTF8

Invoke-Checked 'pptx-editor-engine: build ppt-font-server release' $FontBuild {
  $env:CARGO_TARGET_DIR = Join-Path $FontBuild 'target'
  cargo build --release --manifest-path (Join-Path $FontBuild 'Cargo.toml')
}
Copy-Item -Force (Join-Path $FontBuild 'target\release\rust-backend.exe') (Join-Path $PptOut 'bin\ppt-font-server.exe')
cmd /c rmdir /s /q "`"$FontBuild`"" 2>$null
if (-not $KeepExternalTargets) {
  $t = Join-Path $PptRepo 'rust-engine\target'
  if (Test-Path $t) { cmd /c rmdir /s /q "`"$t`"" 2>$null }
}

Set-Content -Path (Join-Path $PptOut 'plugin.json') -Encoding UTF8 -Value @'
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
'@

Set-Content -Path (Join-Path $PptOut 'SKILL.md') -Encoding UTF8 -Value @'
---
name: ppt
description: 本地 Rust WASM PPTX 演示文稿渲染与编辑工作台模块
---

# PPT 编辑器模块

本模块随 QuickAgent 模块化打包自动启用，提供基于 Rust WASM（`cosmic-text`）与 HTML5 Canvas 的本地 `.pptx` 渲染、编辑、模板预览与导出工作台。

- 点击左侧导航栏的 **PPT 编辑器** 图标即可直接打开工作台；
- 支持加载示例文稿、打开/拖入本地 `.pptx` 文件、编辑页面元素与排版、以及导出修改后的 `.pptx` 文件；
- 系统字体由随包附带的 `ppt-font-server` 本地助手按需自动提供，无需手工启动额外端口。
'@
}

Write-Host "`n==> Summary of bundled_plugins:" -ForegroundColor Green
Get-ChildItem -LiteralPath $BundledRoot -Directory | ForEach-Object {
  $pj = Join-Path $_.FullName 'plugin.json'
  $hasManifest = Test-Path $pj
  $bins = (Get-ChildItem -LiteralPath (Join-Path $_.FullName 'bin') -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name) -join ', '
  Write-Host ("  - {0,-10} manifest={1,-5} bins=[{2}]" -f $_.Name, $hasManifest, $bins)
}
