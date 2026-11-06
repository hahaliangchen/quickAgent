const ppt = require("ppt_master");

// ---- 1) 封面：标题单行化，避免换行溢出（仅调整文本框宽度/字号，固定品牌元素原样保留）
const coverHtml = `<!doctype html><html><head><meta charset="utf-8"><style>*{box-sizing:border-box}html,body{margin:0;width:1280px;height:720px}body{position:relative;background:#ffffff}p{margin:0;flex-shrink:0}img{object-fit:fill}</style></head><body data-page-id="ppt/slides/slide1.xml" data-source-hash="f1b2fe19a7326b6f14f9bc084cbfe841458fa704f0c859fdc1924ff617a39e8b" data-design-revision="5" data-page-role="cover" data-layout-name="红白品牌封面" data-page-fill="layout"><img data-id="ppt/slides/slide1.xml#backdrop" data-kind="asset" data-name="Master and background" data-locked="all" data-purpose="fixed" data-required="true" style="position:absolute;left:0px;top:0px;width:1280px;height:720px" src="design-asset:ppt%2Fslides%2Fslide1.xml%23backdrop" alt="Master and background">
<div data-id="ppt/slides/slide1.xml#2" data-kind="shape" data-name="Rectangle 2" data-locked="all" data-purpose="fixed" style="position:absolute;left:0px;top:0px;width:1280px;height:720px;background-color:#ffffff"></div>
<img data-id="ppt/slides/slide1.xml#3" data-kind="image" data-name="Image 3" data-locked="all" data-purpose="fixed" style="position:absolute;left:755px;top:-6px;width:500px;height:124px" src="design-asset:ppt%2Fslides%2Fslide1.xml%233" alt="Image 3">
<div data-id="ppt/slides/slide1.xml#4" data-kind="shape" data-name="Rectangle 4" data-purpose="content" style="position:absolute;left:156px;top:212px;width:6px;height:354px;background-color:#e60012"></div>
<div data-id="ppt/slides/slide1.xml#5" data-kind="shape" data-name="Rectangle 5" data-purpose="content" style="position:absolute;left:162px;top:212px;width:2px;height:354px;background-color:#e60012"></div>
<div data-id="ppt/slides/slide1.xml#6" data-kind="text" data-name="TextBox 6" data-purpose="content" style="position:absolute;left:180px;top:341px;width:920px;height:72px;display:flex;flex-direction:column;justify-content:flex-start;padding:0px 0px 0px 0px"><p data-paragraph="true" style="font-family:&quot;Noto Sans CJK SC&quot;;font-size:50px;font-weight:700;font-style:normal;margin:0px 0 0px;text-align:center;line-height:62px;white-space:pre-wrap;overflow-wrap:normal"><span style="font-family:&quot;Noto Sans CJK SC&quot;;font-size:50px;color:#d00000;font-weight:700;font-style:normal;letter-spacing:0px">雅砻江知识运营平台二期需求汇报</span></p></div>
<div data-id="ppt/slides/slide1.xml#7" data-kind="text" data-name="TextBox 7" data-purpose="content" style="position:absolute;left:339.984px;top:429.6px;width:600px;height:38.4px;display:flex;flex-direction:column;justify-content:flex-start;padding:0px 0px 0px 0px"><p data-paragraph="true" style="font-family:&quot;Noto Sans CJK SC&quot;;font-size:24px;font-weight:700;font-style:normal;margin:0px 0 0px;text-align:center;line-height:28.8px;white-space:pre-wrap;overflow-wrap:normal"><span style="font-family:&quot;Noto Sans CJK SC&quot;;font-size:24px;color:#333333;font-weight:700;font-style:normal;letter-spacing:0px">一期成果 · 二期需求 · 实施路线</span></p></div>
<img data-id="ppt/slides/slide1.xml#8" data-kind="asset" data-name="Line 8" data-locked="all" data-purpose="fixed" style="position:absolute;left:210px;top:478px;width:310px;height:1px" src="design-asset:ppt%2Fslides%2Fslide1.xml%238" alt="Line 8">
<img data-id="ppt/slides/slide1.xml#9" data-kind="asset" data-name="Ellipse 9" data-locked="all" data-purpose="fixed" style="position:absolute;left:516px;top:474px;width:8px;height:8px" src="design-asset:ppt%2Fslides%2Fslide1.xml%239" alt="Ellipse 9">
<img data-id="ppt/slides/slide1.xml#10" data-kind="asset" data-name="Ellipse 10" data-locked="all" data-purpose="fixed" style="position:absolute;left:529px;top:475px;width:6px;height:6px" src="design-asset:ppt%2Fslides%2Fslide1.xml%2310" alt="Ellipse 10">
<div data-id="ppt/slides/slide1.xml#11" data-kind="text" data-name="TextBox 11" data-purpose="content" style="position:absolute;left:226.6px;top:511.1px;width:300px;height:54.4px;display:flex;flex-direction:column;justify-content:flex-start;padding:0px 0px 0px 0px"><p data-paragraph="true" style="font-family:&quot;Noto Sans CJK SC&quot;;font-size:34px;font-weight:700;font-style:normal;margin:0px 0 0px;text-align:left;line-height:40.8px;white-space:pre-wrap;overflow-wrap:normal"><span style="font-family:&quot;Noto Sans CJK SC&quot;;font-size:34px;color:#111111;font-weight:700;font-style:normal;letter-spacing:0px">项目组</span></p></div>
<div data-id="ppt/slides/slide1.xml#12" data-kind="text" data-name="TextBox 12" data-purpose="content" style="position:absolute;left:227px;top:570.5px;width:300px;height:48px;display:flex;flex-direction:column;justify-content:flex-start;padding:0px 0px 0px 0px"><p data-paragraph="true" style="font-family:&quot;Noto Sans CJK SC&quot;;font-size:30px;font-weight:700;font-style:normal;margin:0px 0 0px;text-align:left;line-height:36px;white-space:pre-wrap;overflow-wrap:normal"><span style="font-family:&quot;Noto Sans CJK SC&quot;;font-size:30px;color:#111111;font-weight:700;font-style:normal;letter-spacing:0px">2026 年 11 月</span></p></div></body></html>`;

const r1 = await ppt.apply_html({ html: coverHtml });
console.log("=== APPLY p1 ===");
console.log(JSON.stringify(r1).slice(0, 3500));

// ---- 2) 探测第 4 页各块的底色，用于决定图标颜色
const d4 = await ppt.get_design({ page: 4, html: true });
const html = d4.data.html || "";
console.log("=== p4 html length ===", html.length, "imgs:", (html.match(/<img/g) || []).length, "svgs:", (html.match(/<svg/g) || []).length);
const re = /data-kind="([a-z]+)"\s+data-name="([^"]*)"[^>]*?style="([^"]*)"/g;
const rows = [];
let m;
while ((m = re.exec(html))) {
  const st = m[3];
  const bg = (st.match(/background-color:([^;"]+)/) || [])[1];
  const bd = (st.match(/border:([^;"]+)/) || [])[1];
  const br = (st.match(/border-radius:([^;"]+)/) || [])[1];
  rows.push(`${rows.length} ${m[1]} ${m[2]} bg=${bg || "-"} bd=${bd || "-"} r=${br || "-"}`);
}
console.log("=== p4 shapes ===");
console.log(rows.slice(0, 90).join("\n").slice(0, 9000));
