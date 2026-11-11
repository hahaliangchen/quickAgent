#!/usr/bin/env bash
set -Eeuo pipefail
[[ "$(uname -m)" == aarch64 ]]
[[ "$(getconf GNU_LIBC_VERSION)" == 'glibc 2.28' ]]
rustc --version
cargo --version
rustup target list --installed | grep -qx wasm32-unknown-unknown
node --version
npm --version
go version
python --version
wasm-pack --version
rg --version | head -n 1
command -v gcc g++ make cmake pkg-config git bwrap pdftotext pdftoppm
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
printf 'int main(void) { return 0; }\n' > "$work/hello.c"
gcc "$work/hello.c" -o "$work/hello-c"
"$work/hello-c"
printf 'fn main() { println!("Rust ARM64 OK"); }\n' > "$work/hello.rs"
rustc "$work/hello.rs" -o "$work/hello-rust"
"$work/hello-rust"
printf '#[unsafe(no_mangle)] pub extern "C" fn answer() -> u32 { 42 }\n' > "$work/wasm.rs"
rustc --edition 2024 --target wasm32-unknown-unknown --crate-type cdylib "$work/wasm.rs" -o "$work/check.wasm"
node -e 'const fs=require("fs"); const m=new WebAssembly.Instance(new WebAssembly.Module(fs.readFileSync(process.argv[1]))); if(m.exports.answer()!==42)process.exit(1)' "$work/check.wasm"
printf 'package main\nimport "fmt"\nfunc main() { fmt.Println("Go ARM64 OK") }\n' > "$work/hello.go"
CGO_ENABLED=0 go build -o "$work/hello-go" "$work/hello.go"
"$work/hello-go"
python - "$work" <<'PY'
import io, sys
from pathlib import Path
import cairosvg, numpy, openai, requests, svglib, reportlab
from google import genai
from PIL import Image
from pptx import Presentation
work = Path(sys.argv[1])
Presentation().save(work / 'check.pptx')
assert len(Presentation(work / 'check.pptx').slides) == 0
png = cairosvg.svg2png(bytestring=b'<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><rect width="64" height="32" fill="red"/></svg>')
assert Image.open(io.BytesIO(png)).size == (64, 32)
print('Python PPTX, Pillow, Cairo/SVG and image clients: OK')
PY
(cd "$work" && node /opt/environment/check-tools.cjs "$work/canvas.png")
printf 'Kylin ARM64 tool environment: all checks passed.\n'
