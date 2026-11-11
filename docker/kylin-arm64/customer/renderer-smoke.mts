import { readFileSync, writeFileSync } from 'node:fs';
import { projectPage } from '/opt/ppt-engine/src/design/projection';

const base = readFileSync('/opt/ppt-engine/tools/templates/blank.pptx').toString('base64');
async function design(body: unknown) {
    const response = await fetch('http://127.0.0.1:8787/design', {method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify(body)});
    const value = await response.json() as any;
    if (!response.ok) throw new Error(JSON.stringify(value));
    return value.design;
}
const initial = await design({operation:'sync', pptxBase64:base});
const html = projectPage(initial, initial.pages[0].id).replace('</body>',
    '<div data-kind="text" style="position:absolute;left:80px;top:80px;width:1000px;height:120px"><p style="font-family:Noto Sans CJK SC;font-size:42px;line-height:56px;margin:0">麒麟 ARM64 客户部署验证</p></div></body>');
const generated = await design({operation:'apply', design:initial, expectedRevision:initial.revision, html});
const bytes = Buffer.from(generated.source.pptxBase64, 'base64');
writeFileSync('/tmp/customer-render.pptx',bytes);
const response = await fetch('http://127.0.0.1:8787/render?page=1&width=960', {method:'POST', body:bytes});
if (!response.ok) throw new Error(await response.text());
const png = Buffer.from(await response.arrayBuffer());
if (png.subarray(0,8).toString('hex') !== '89504e470d0a1a0a') throw new Error('Invalid render output');
writeFileSync('/tmp/customer-render.png',png);
console.log(JSON.stringify({html_design:true, wasm_pptx_roundtrip:true, chinese_png:true, pages:generated.pages.length, png_bytes:png.length}));
