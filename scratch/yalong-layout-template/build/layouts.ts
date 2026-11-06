// Content-area HTML using only CSS the converter maps to native PPT paint: linear gradients, one outer shadow,
// dashed borders, top/bottom corner pairs and polygon clip-paths. No pseudo elements, radial gradients or transforms.
export const RED = '#E60012', PINK = '#FDEDEE';
const HEAD = 'background-image:linear-gradient(90deg, #E60012 0%, #C9000F 65%, #EC4A57 100%)';
const DEEP = 'background-image:linear-gradient(90deg, #B8000E 0%, #E60012 50%, #B8000E 100%)';
const CARD_SHADOW = 'box-shadow:0px 1px 4px rgba(0, 0, 0, 0.09)';
const PANEL_SHADOW = 'box-shadow:0px 2px 8px rgba(0, 0, 0, 0.1)';
const FONT = "font-family:'Microsoft YaHei'";

export const P = (t: string, o: {size?: number; lh?: number; color?: string; w?: number; align?: string} = {}) => {
  const size = o.size ?? 13, lh = o.lh ?? Math.round(size * 1.5);
  return `<p style="${FONT};font-size:${size}px;line-height:${lh}px;color:${o.color ?? '#333333'};font-weight:${o.w ?? 400};text-align:${o.align ?? 'left'}">${t}</p>`;
};
export const hl = (t: string) => `<span style="color:${RED};font-weight:700">${t}</span>`;
const dot = `<span style="color:${RED};font-size:9px">■</span>&nbsp;`;
const bullets = (items: string[], size = 12.5, lh = 19) => items.map(i => P(dot + i, {size, lh})).join('');
const h4 = (t: string, size = 15) => P(t, {size, lh: Math.round(size * 1.35), color: RED, w: 700});

const T = (inner: string, style = '') => `<div data-kind="text" style="position:relative;${style}">${inner}</div>`;
const S = (style: string) => `<div data-kind="shape" style="position:absolute;${style}"></div>`;
const bg = (style: string) => S(`left:0px;top:0px;width:100%;height:100%;${style}`);
/** A whole block the AI copies or deletes as one group; build.ts reads `data-block` back to annotate it. */
const G = (block: string, style: string, inner: string) => `<div data-kind="group" data-block="${block}" style="position:relative;${style}">${inner}</div>`;
/** A styled block: its background is its own shape, drawn before the contents. Named blocks become groups. */
const box = (style: string, fill: string, inner: string, block?: string) => block ? G(block, style, bg(fill) + inner) : `<div style="position:relative;${style}">${bg(fill)}${inner}</div>`;

type IcoOpt = {round?: boolean; white?: boolean; plain?: boolean; surface?: boolean};
export const ico = (size = 46, o: IcoOpt = {}) => {
  const q = Math.round(size * 0.26), r = o.round ? size / 2 : Math.round(size * 0.22);
  const fill = o.white ? 'background-color:rgba(255, 255, 255, 0.2)' : o.surface ? `background-color:#FFFFFF;${CARD_SHADOW}` : `background-color:${PINK}`;
  return `<div data-kind="group" data-block="图标占位" style="position:relative;flex:none;width:${size}px;height:${size}px">` +
    (o.plain ? '' : S(`left:0px;top:0px;width:${size}px;height:${size}px;${fill};border-radius:${r}px`)) +
    S(`left:${q}px;top:${q}px;width:${size - 2 * q}px;height:${size - 2 * q}px;border:2px dashed ${o.white ? '#FFFFFF' : RED};border-radius:50%`) + `</div>`;
};

/** `sep` marks a divider between repeated items; build.ts turns it into the set's separator slot. */
const vr = (color = RED, w = 2, sep = false) => `<div style="position:relative;flex:none;align-self:stretch;width:${w}px">${S(`left:0px;top:0px;width:${w}px;height:100%;background-color:${color}`).replace('data-kind="shape"', sep ? 'data-kind="shape" data-sep=""' : 'data-kind="shape"')}</div>`;
const sepT = (inner: string, style: string) => T(inner, style).replace('data-kind="text"', 'data-kind="text" data-sep=""');
const chev = (size = 22) => sepT(P('»', {size, lh: size + 4, color: RED, w: 900, align: 'center'}), 'flex:none;align-self:center;width:14px');
const arrow = () => sepT(P('→', {size: 18, lh: 22, color: RED, w: 900, align: 'center'}), 'flex:none;align-self:center');

const card = (inner: string, style = '') => box(`flex:1;min-height:0;min-width:0;display:flex;gap:11px;align-items:center;padding:8px 10px;${style}`, `background-color:#FFFFFF;border-radius:6px;${CARD_SHADOW}`, inner, '要点卡');
const item = (title: string, lines: string[], size = 46, tsize = 15) => card(ico(size) + T(h4(title, tsize) + bullets(lines), 'flex:1;min-width:0'));

export const panel = (title: string, body: string | string[], o: {flex?: string; hd?: number; fs?: number; dir?: string; gap?: number} = {}) => {
  const hd = o.hd ?? 40, fs = o.fs ?? 18;
  return `<div data-kind="group" data-block="红头卡片" style="position:relative;flex:${o.flex ?? '1'};min-width:0;min-height:0;display:flex;flex-direction:column">` +
    bg(`background-image:linear-gradient(180deg, #FFFFFF 0%, #FDF3F3 100%);border-radius:8px;${PANEL_SHADOW}`) +
    `<div style="position:relative;flex:none;height:${hd}px;display:flex;align-items:center;gap:10px;padding:0px 14px">` +
    S(`left:0px;top:0px;width:100%;height:${hd}px;${HEAD};border-radius:8px 8px 0px 0px`) +
    ico(hd > 44 ? 32 : 28, {white: true}) + T(P(title, {size: fs, lh: Math.round(fs * 1.3), color: '#FFFFFF', w: 700}), 'flex:1;min-width:0') + `</div>` +
    `<div style="position:relative;flex:1;min-height:0;padding:9px;display:flex;flex-direction:${o.dir ?? 'column'};gap:${o.gap ?? 8}px">${Array.isArray(body) ? body.join('') : body}</div></div>`;
};

export const intro = (text: string, small = false) => box(`flex:none;display:flex;align-items:center;gap:14px;padding:${small ? 6 : 9}px 18px`, `background-image:linear-gradient(90deg, #F1F1F3 0%, #FAFAFB 100%);border-radius:8px;${CARD_SHADOW}`,
  ico(small ? 30 : 46) + vr() + T(P(text, {size: 15, lh: 24, color: '#222222'}), 'flex:1;min-width:0'), '导语条');

export const ribbon = (t: string, size = 18) => `<div data-kind="group" data-block="飘带标题" style="position:relative;flex:none;display:flex;justify-content:center">` +
  S(`left:12%;bottom:0px;width:76%;height:2px;background-image:linear-gradient(90deg, rgba(230, 0, 18, 0) 0%, #E60012 50%, rgba(230, 0, 18, 0) 100%)`) +
  box('padding:4px 64px', `${DEEP};clip-path:polygon(5% 0%, 95% 0%, 100% 100%, 0% 100%)`, T(P(t, {size, lh: size + 6, color: '#FFFFFF', w: 700, align: 'center'}))) + `</div>`;

export const stage = (labels: string[], round = true) => `<div style="position:relative;flex:none;display:flex;justify-content:center;gap:22px;padding:2px 0px 8px">` +
  S('left:18%;bottom:0px;width:64%;height:30px;background-image:linear-gradient(180deg, rgba(253, 225, 228, 0) 0%, #FDE1E4 100%);border-radius:50%') +
  labels.map(l => `<div data-kind="group" data-block="图标场景项" style="position:relative;display:flex;flex-direction:column;align-items:center;gap:5px;width:100px">${ico(52, {round, surface: true})}${T(P(l, {size: 15, lh: 20, color: '#222222', w: 700, align: 'center'}))}</div>`).join('') + `</div>`;

export const concl = (t: string) => box('flex:none;height:40px;display:flex;align-items:center', 'background-image:linear-gradient(90deg, #FDE3E6 0%, #FFF4F5 70%, #FDE3E6 100%);border-radius:6px',
  box('flex:none;width:76px;height:40px;display:flex;align-items:center;justify-content:center;padding-right:8px', `background-color:${RED};clip-path:polygon(0% 0%, 100% 0%, 84% 100%, 0% 100%)`, T(P('»', {size: 20, lh: 24, color: '#FFFFFF', w: 900, align: 'center'})).replace('data-kind="text"', 'data-kind="text" data-deco=""')) +
  T(P(t, {size: 18, lh: 24, color: RED, w: 700, align: 'center'}), 'flex:1;padding:0px 18px'), '结论条');

const vbar = (items: string[], round = true, size = 17, h = 56) => box(`flex:none;height:${h}px;display:flex;align-items:center`, `background-image:linear-gradient(90deg, #FDEEF0 0%, #FFFFFF 100%);border-radius:6px;${CARD_SHADOW}`,
  items.map((t, i) => (i ? vr('#E9B9BE', 1, true) : '') + G('价值项', 'flex:1;display:flex;align-items:center;justify-content:center;gap:12px', ico(size > 15 ? 40 : 28, {round, plain: size <= 15}) + T(P(t, {size, lh: size + 6, color: '#222222', w: 700})))).join(''), '价值横条');

const band = (t: string, small = false) => box(`flex:none;height:${small ? 34 : 44}px;display:flex;align-items:center;gap:12px;padding:0px 18px`, 'background-image:linear-gradient(90deg, #B8000E 0%, #E60012 40%, #EF5662 100%);border-radius:6px',
  ico(small ? 26 : 30, {white: true}) + T(P(t, {size: small ? 17 : 22, lh: small ? 22 : 28, color: '#FFFFFF', w: 700})), '红色分节条');

/**
 * Brand TOC rows in the original template's place and colours (number tile + title bar, the first one highlighted),
 * as one repeatable block so fill_layout changes the entry count. Rows have no sub-items: rows of unequal height
 * cannot be re-spaced evenly. `data-field` names the texts for build.ts.
 */
const field = (name: string, inner: string, style: string) => T(inner, style).replace('data-kind="text"', `data-kind="text" data-field="${name}"`);
export const tocRows = `<div style="position:absolute;left:450px;top:162px;width:740px;height:488px;display:flex;flex-direction:column;justify-content:space-between">` +
  ['一', '二', '三', '四'].map((n, i) => G('目录项', 'flex:none;height:80px;display:flex;gap:60px',
    box('flex:none;width:140px;display:flex;align-items:center', `background-color:${i ? '#8C8C8C' : '#D40000'}`, field('num', P(n, {size: 34, lh: 41, color: '#FFFFFF', w: 700, align: 'center'}), 'flex:1')) +
    box('flex:1;display:flex;align-items:center;padding:0px 18px', `background-color:${i ? '#8C8C8C' : '#D40000'}`, field('title', P(`目录条目${n}`, {size: 28, lh: 34, color: '#FFFFFF', w: 700, align: 'center'}), 'flex:1')))).join('') + `</div>`;

const row = (inner: string, style = '') => `<div style="position:relative;display:flex;gap:6px;min-height:0;${style}">${inner}</div>`;
const repeat = <X,>(n: number, f: (i: number) => X) => Array.from({length: n}, (_, i) => f(i));
const zh = ['一', '二', '三', '四', '五', '六', '七', '八', '九'];

const cell = (label: string) => box('flex:1;min-width:0;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:3px', `background-color:#FFFFFF;border-radius:5px;${CARD_SHADOW}`,
  ico(26, {plain: true}) + T(P(label, {size: 13, lh: 17, color: '#222222', w: 700, align: 'center'})), '能力格');

export interface Layout {name: string; title: string; note: string; html: string}

export const layouts: Layout[] = [
  {
    name: '四栏递进', title: '页面标题——副标题占位',
    note: '四栏递进版式参考：栏数、每栏卡片数、场景图标数量均按内容增减，可两栏到五栏；保持红色标题栏卡片、白底要点卡、导语条、飘带标题和底部结论条的样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro(`导语占位文字，概括本页背景与要点，约两行。此处为说明性文字，用于交代现状、问题与方向之间的关系，${hl('关键结论用红色加粗强调。')}`) +
      row(repeat(4, c => panel(`栏目标题${zh[c]}`, repeat(c ? 4 : 3, i => item(`${c < 2 ? `${i + 1}. ` : ''}要点标题`, c ? ['说明文字占位，约一到两行'] : ['说明文字占位，约一到两行内容', '说明文字占位'])))).join(chev()), 'flex:1') +
      ribbon('飘带标题占位') + stage(repeat(5, i => `场景${zh[i]}`), false) + concl('结论句占位：一句话总结本页核心观点，红色加粗居中显示。')
  },
  {
    name: '分层架构', title: '页面标题——副标题占位',
    note: '分层架构版式参考：层数和每层能力项数量按内容增减，右侧支撑栏可保留或去掉；保持红色层标签、浅底能力格、白色能力卡、价值横条的样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro(`导语占位文字，说明总体架构的定位，${hl('关键链路用红色加粗强调')}，并列出支撑的主要业务场景，约两行。`) +
      row(`<div style="position:relative;flex:1;min-width:0;display:flex;flex-direction:column;gap:6px">` +
        [8, 6, 6, 8, 9].map((n, l) => G('架构层', 'flex:1;display:flex;gap:8px;min-height:0', 
          box('flex:none;width:190px;display:flex;align-items:center;gap:10px;padding:0px 14px', 'background-image:linear-gradient(90deg, #B8000E 0%, #E60012 100%);clip-path:polygon(0% 0%, 90% 0%, 100% 50%, 90% 100%, 0% 100%)', ico(28, {white: true}) + T(P(`第${zh[l]}层名称`, {size: 17, lh: 22, color: '#FFFFFF', w: 700})), '层标签') +
          box('flex:1;min-width:0;display:flex;gap:6px;padding:4px', 'background-image:linear-gradient(90deg, #FBEAEB 0%, #FAFAFA 100%);border-radius:6px', repeat(n, () => cell('能力项')).join(''), '能力条'))).join('') + `</div>` +
        panel('侧栏标题', repeat(3, i => card(ico(46) + T(P(`支撑能力${zh[i]}`, {size: 16, lh: 22, color: '#222222', w: 700}), 'flex:1'))), {flex: '0 0 200px', fs: 16}), 'flex:1;gap:12px') +
      ribbon('飘带标题占位') + vbar(repeat(4, i => `价值点占位${zh[i]}`))
  },
  {
    name: '双栏演进', title: '页面标题——副标题占位',
    note: '双栏演进版式参考：通栏方向横幅、左右两栏与中间过渡箭头；每栏条目数按内容增减，也可改为单栏或三栏；保持红色横幅、红色标题栏卡片、大图标要点卡和结论条样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro('导语占位文字：说明本页包含的两部分内容及其关系，第一部分为现状基础，第二部分为后续规划，约两行。') +
      band('方向标签：通栏核心方向占位文字') +
      row(panel('左栏标题占位（节点）', repeat(3, () => card(ico(80) + T(h4('要点标题', 19) + bullets(['说明文字占位，约两行内容，描述已完成的建设成果与范围']), 'flex:1;min-width:0')))) +
        box('flex:none;width:130px;align-self:center;height:220px;display:flex;align-items:center;justify-content:center;padding:0px 34px 0px 6px', 'background-image:linear-gradient(90deg, #FFFFFF 0%, #FBD3D7 100%);clip-path:polygon(0% 18%, 62% 18%, 62% 0%, 100% 50%, 62% 100%, 62% 82%, 0% 82%)',
          T(P('过渡说明<br>占位', {size: 15, lh: 22, color: RED, w: 700, align: 'center'})), '过渡箭头') +
        panel('右栏标题占位（规划）', repeat(4, () => card(ico(60) + T(h4('要点标题', 18), 'flex:0 0 130px') + vr('#EEEEEE', 1) + T(bullets(['说明文字占位，约一到两行', '说明文字占位，约一到两行']), 'flex:1;min-width:0')))), 'flex:1;gap:12px') +
      concl('结论句占位：一句话串联两部分内容的关系。')
  },
  {
    name: '四栏能力与流程', title: '页面标题——副标题占位',
    note: '能力模块版式参考：模块数量、每个模块的功能项、流程环节数、价值点数按内容增减；保持红色标题栏卡片、大图标加功能清单、圆形环节链和价值横条的样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro(`导语占位文字：说明本页对象的定位与工作重点，围绕${hl('“目标”')}形成${hl('若干项能力')}的基础框架，约两行。`) +
      row(repeat(4, c => panel(`能力模块${zh[c]}`, ico(80) + T(bullets(repeat(c === 2 ? 7 : 6, () => '功能项占位'), 15, 28), 'flex:1;min-width:0'), {dir: 'row', gap: 12})).join(chev()), 'flex:1') +
      row(repeat(5, i => `<div data-kind="group" data-block="图标环节项" style="position:relative;display:flex;flex-direction:column;align-items:center;gap:3px">${ico(44, {round: true, surface: true})}${T(P(`环节${zh[i]}`, {size: 12, lh: 16, color: '#222222', w: 700, align: 'center'}))}</div>`).join(arrow()), 'flex:none;justify-content:center;gap:18px') +
      `<div style="position:relative;flex:none;display:flex;flex-direction:column">` +
        box('flex:none;align-self:flex-start;height:32px;display:flex;align-items:center;padding:0px 40px 0px 14px', `background-color:${RED};clip-path:polygon(0% 0%, 100% 0%, 92% 100%, 0% 100%)`, T(P('»&nbsp;&nbsp;价值标题占位', {size: 18, lh: 24, color: '#FFFFFF', w: 700})), '价值标签') +
        vbar(repeat(4, i => `价值点${zh[i]}`), false) + `</div>`
  },
  {
    name: '痛点-流程-成果', title: '页面标题——副标题占位',
    note: '三栏版式参考：左为问题要点、中为编号步骤流水线、右为数据成果格；问题条数、步骤数、指标数按内容增减，栏宽可调；保持红色标题栏、编号圆圈步骤条、大号红色数字的样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro(`${hl('导语占位：本环节的定位一句话。')}基于前期工作的说明文字占位，相关工作已形成从 A 到 B 再到 C 的${hl('完整闭环。')}`) +
      row(panel('栏目标题一', repeat(3, i => card(ico(60) + T(h4(`短语${zh[i]}`, 19) + bullets(['问题描述占位，约两到三行文字，说明现象与影响']), 'flex:1;min-width:0')))) +
        panel('栏目标题二（步骤）', repeat(6, i => card(
          box('flex:none;width:24px;height:24px;display:flex;align-items:center;justify-content:center', `border:1.5px solid ${RED};border-radius:12px`, T(P(String(i + 1), {size: 13, lh: 16, color: RED, w: 700, align: 'center'}))) +
          ico(28, {plain: true}) + T(P('步骤名称', {size: 15, lh: 20, color: RED, w: 700}), 'flex:0 0 76px') + vr('#EEEEEE', 1) + T(P('步骤说明占位，约一到两行文字', {size: 12, lh: 17, color: '#444444'}), 'flex:1;min-width:0'), 'gap:9px;padding:0px 10px')), {flex: '1.3', gap: 6}) +
        panel('栏目标题三（成果）', `<div style="position:relative;flex:1;display:grid;grid-template-columns:1fr 1fr;gap:8px">` +
          ['0000', '000', '00', '00', '100%', '100%'].map(n => card(ico(46) + T(P('指标名称', {size: 13, lh: 18, color: '#222222', w: 700}) + P(n.endsWith('%') ? n : `${n}<span style="font-size:13px;color:#222222">&nbsp;单位</span>`, {size: 30, lh: 36, color: RED, w: 900}), 'flex:1;min-width:0'), 'gap:10px')).join('') + `</div>`, {flex: '1.2'}), 'flex:1;gap:10px') +
      ribbon('飘带标题占位') + stage(repeat(4, i => `用途${zh[i]}`), false) + concl('结论句占位：下一步工作方向一句话。')
  },
  {
    name: '五步流程', title: '页面标题——副标题占位',
    note: '五步流程版式参考：步骤数按内容增减，各步骤卡内可用提问气泡、要点卡、对象小格等不同组合；底部基础卡数量按内容增减；保持红色双行标题栏、白色要点卡、粉边气泡和结论条样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro(`导语占位文字：本场景是现有能力的直接应用，旨在支持若干业务下的${hl('关键诉求一、关键诉求二和关键诉求三。')}`) +
      row([
        panel('1. 步骤标题', repeat(3, () => `<div data-kind="group" data-block="提问气泡" style="position:relative;flex:1;display:flex;gap:8px;align-items:center">${ico(30, {round: true})}${box('flex:1;min-width:0;padding:6px 10px', 'background-color:#FFFFFF;border:1px solid #F1D2D5;border-radius:8px', T(P('“示例问题占位文字？”', {size: 14, lh: 20, color: '#222222'})))}</div>`).join(''), {hd: 52, fs: 16}),
        panel('2. 步骤标题<br>（两行）', repeat(2, i => card(ico(60) + T(P(i ? `说明占位，选择相匹配的${hl('对象范围')}。` : `说明占位，${hl('关键词一、关键词二')}等类型。`, {size: 14, lh: 21, color: '#444444'}), 'flex:1;min-width:0'))), {hd: 52, fs: 16}),
        panel('3. 步骤标题<br>（两行）', `<div style="position:relative;flex:1.4;display:grid;grid-template-columns:1fr 1fr;gap:6px">${repeat(4, i => cell(`对象${zh[i]}`)).join('')}</div>` + card(ico(46) + T(P(`基于${hl('某机制')}，说明占位文字约两行。`, {size: 12.5, lh: 19, color: '#444444'}), 'flex:1;min-width:0')), {hd: 52, fs: 16}),
        panel('4. 步骤标题<br>（两行）', repeat(4, () => card(ico(30) + T(P(`输出说明${hl('关键词')}占位`, {size: 12.5, lh: 19, color: '#444444'}), 'flex:1;min-width:0'))), {hd: 52, fs: 16}),
        panel('5. 步骤标题', repeat(4, () => card(ico(30) + T(P(`${hl('动词')}价值说明占位`, {size: 12.5, lh: 19, color: '#444444'}), 'flex:1;min-width:0'))), {hd: 52, fs: 16}),
      ].join(chev()), 'flex:1') +
      ribbon('飘带标题占位') +
      row(repeat(3, i => card(`<div style="position:relative;flex:none;display:flex;flex-direction:column;align-items:center;gap:4px">${ico(46)}${T(P(`基础${zh[i]}`, {size: 15, lh: 20, color: RED, w: 700, align: 'center'}))}</div>` + vr('#EEEEEE', 1) + T(P('说明占位文字，约三行，描述该方面提供的核心能力。', {size: 12.5, lh: 19, color: '#444444'}), 'flex:1;min-width:0'), 'flex:0 0 300px')).join(''), 'flex:none;height:84px;gap:16px;justify-content:center') +
      concl('结论句占位：本页内容的最终价值一句话。')
  },
  {
    name: '问题与方向', title: '页面标题——副标题占位',
    note: '问题到方向版式参考：栏数、每栏问题卡数按内容增减；每栏底部粉底方向框可保留或去掉；下方聚焦方向数按内容增减；保持红色双行标题栏、大图标要点卡、粉底方向框的样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro(`导语占位文字：在前期工作基本落地后，下一步的挑战不再是简单的“某件事”，而是将能力向${hl('更高层级的统一方向')}延伸。`) +
      row(repeat(4, c => panel(`问题短语${zh[c]}，<br>问题短语补充`,
        card(ico(60) + T(bullets(['现象描述占位，约三行文字，说明问题产生的原因与范围']), 'flex:1;min-width:0')) +
        card(ico(60) + T(bullets(['影响描述占位，约三行文字，说明问题造成的后果']), 'flex:1;min-width:0')) +
        box('flex:0.8;display:flex;align-items:center;padding:8px 10px', `background-color:${PINK};border-radius:6px`, T(P(`${hl('»&nbsp;方向：方向占位，')}补充说明占位文字。`, {size: 12.5, lh: 19, color: '#444444'}), 'flex:1'), '方向框'),
        {hd: 52, fs: 16})).join(chev()), 'flex:1') +
      ribbon('飘带标题占位') +
      row(repeat(4, i => (i ? vr('#EECCCC', 1, true) : '') + `<div data-kind="group" data-block="方向项" style="position:relative;flex:1;min-width:0;display:flex;gap:10px;align-items:center;padding:0px 12px">${ico(60, {round: true, surface: true})}${T(h4(`方向${zh[i]}`, 17) + P('说明占位文字，约两到三行，概括该方向的内涵', {size: 12, lh: 18, color: '#444444'}), 'flex:1;min-width:0')}</div>`).join(''), 'flex:none;height:90px;gap:0px;padding:4px 60px')
  },
  {
    name: '概念与关系链', title: '页面标题——副标题占位',
    note: '三栏关系版式参考：左为原因要点、中为概念说明与节点关系链、右为规划清单；条目数、概念数、节点与分支数量按内容增减；保持红色标题栏、白色要点卡、圆形节点链和胶囊分支的样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro(`导语占位文字：在某场景中，问题的解答不仅依赖 A，还需要对${hl('对象一、对象二、对象三')}等及其关系有清晰理解。因此，应在现有基础上${hl('引入新的能力。')}`) +
      row(panel('为什么占位标题', repeat(4, () => card(ico(60) + T(bullets(['原因说明占位，约两到三行文字，配合括号举例（示例一、示例二）']), 'flex:1;min-width:0')))) + chev() +
        panel('作用说明占位标题', repeat(2, i => card(box('flex:0 0 150px;display:flex;align-items:center;gap:8px;padding:10px', 'background-color:#FFFFFF;border:1px solid #F3C4C8;border-radius:6px', ico(30) + T(P(`${'AB'[i]}. 概念${zh[i]}`, {size: 17, lh: 22, color: RED, w: 700})), '概念标签') +
            T(bullets(i ? ['作用说明占位，约两行文字，描述它支撑哪些应用'] : ['定义说明占位，约一行文字', '示例说明占位，约一行文字']), 'flex:1;min-width:0'), 'flex:none;height:64px')).join('') +
          box('flex:1;display:flex;flex-direction:column;justify-content:center;gap:12px', 'background-color:#FFFFFF;border:1px solid #F1DFE1;border-radius:6px',
            row(repeat(6, i => `<div data-kind="group" data-block="图标环节项" style="position:relative;display:flex;flex-direction:column;align-items:center;gap:3px">${ico(44, {round: true, surface: true})}${T(P(`节点${zh[i]}`, {size: 12, lh: 16, color: '#222222', w: 700, align: 'center'}))}</div>`).join(arrow()), 'justify-content:center') +
            row(repeat(2, i => box('display:flex;align-items:center;gap:6px;padding:4px 14px', 'background-color:#FFFFFF;border:1px solid #F3C4C8;border-radius:16px', ico(24, {plain: true}) + T(P(`分支${zh[i]}`, {size: 13, lh: 18, color: '#222222', w: 700})), '分支胶囊')).join(''), 'justify-content:center;gap:40px'), '关系链框'),
          {flex: '1.4'}) + chev() +
        panel('规划占位标题', repeat(4, () => card(ico(60) + T(P(dot + '规划事项占位', {size: 14, lh: 20, color: '#222222', w: 700}), 'flex:1;min-width:0'))), {flex: '0.95'}), 'flex:1;gap:4px') +
      stage(repeat(6, i => `场景${zh[i]}`)) + concl('结论句占位：三者各自的作用及结合后的效果一句话。')
  },
  {
    name: '链路与清单', title: '页面标题——副标题占位',
    note: '链路加清单版式参考：左侧宽面板为环节链路，环节数按内容增减；右侧为事项清单，条数按内容增减；两块宽度比例可调；保持红色标题栏、竖排环节卡、左侧红线事项卡的样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro(`导语占位文字：随着某类参与方持续增加，平台需要从 A 升级为管理${hl('“核心问题占位文字，用引号与红色强调”。')}`) +
      row(panel('左侧主题占位标题', repeat(5, () => card(
          `<div style="position:relative;align-self:center">${ico(60, {plain: true})}</div>` + T(P('环节名称', {size: 15, lh: 20, color: RED, w: 700, align: 'center'}) + P('（补充 / 说明 / 范围）', {size: 12, lh: 17, color: RED, align: 'center'})) + T(bullets(['说明占位，约两到三行文字', '说明占位，约两到三行文字'])),
          'flex-direction:column;align-items:stretch;justify-content:flex-start;gap:6px;padding:12px 8px')).join(chev()), {flex: '2.1', hd: 46, fs: 20, dir: 'row', gap: 4}) +
        panel('右侧主题占位标题', repeat(4, () => card(ico(60, {plain: true}) + vr() + T(h4('事项标题') + P('说明占位文字，约一到两行', {size: 12.5, lh: 19, color: '#444444'}), 'flex:1;min-width:0'))), {hd: 46, fs: 20}), 'flex:1;gap:16px') +
      stage(repeat(6, i => `场景${zh[i]}`)) + concl('结论句占位：本页管理原则一句话。')
  },
  {
    name: '方案与路线', title: '页面标题——副标题占位',
    note: '方案与路线版式参考：上段为方案步骤，中段为分类对象，下段为阶段卡与时间轴，再下为输出条，最底部为结论条；各段的步骤数、类别数、阶段数按内容增减，也可只保留其中两段；结论条放本页一句话结论，原句完整写入，不拆成短标签；保持红色分节通栏、红色标题栏卡片、时间轴节点、输出条和结论条样式；图标为占位，按含义替换。通用规则：文字全部替换为实际内容，没有对应内容的块连同图标一并删除；块的样式（红头卡片、白色要点卡、导语条、飘带标题、结论条等）保持不变，块的数量和行列按内容量决定，例如模板里三行五列的格子，实际只有四项时改成 2×2；每个图标占位都用按该块含义生成的 SVG 图标替换。',
    html: intro(`导语占位文字：后续建设${hl('不应简单如何')}，而是在现有基础上进行${hl('评估')}，按需选择${hl('方式一或方式二')}，逐步纳入统一体系。`, true) +
      band('分节标题一占位', true) +
      row(repeat(4, i => panel(`${i + 1}. 步骤名称`, ico(56, {plain: true}) + T(bullets(['说明占位文字，约两到三行，描述该步骤的工作对象与输出']), 'flex:1;min-width:0'), {hd: 32, fs: 16, dir: 'row', gap: 10})).join(chev()), 'flex:none;height:104px') +
      ribbon('飘带标题占位', 16) +
      row(repeat(3, i => item(`${'ABC'[i]}. 类别名称`, ['说明占位文字，约两行，描述该类别的特征与处理方式'])).join(''), 'flex:none;height:66px;gap:14px') +
      band('分节标题二占位', true) +
      row(repeat(4, i => card(ico(56, {plain: true}) + T(h4(`阶段${i + 1}：时间`) + bullets(['阶段任务占位，约两行文字']), 'flex:1;min-width:0'))).join(''), 'flex:1;gap:12px;padding:0px 10px') +
      `<div style="position:relative;flex:none;height:40px;display:flex;justify-content:space-around">${S('left:30px;top:8px;width:1176px;height:2px;background-image:linear-gradient(90deg, #F4A4AB 0%, #E60012 100%)')}` +
        repeat(4, i => `<div data-kind="group" data-block="时间轴节点" style="position:relative;display:flex;flex-direction:column;align-items:center;gap:3px"><div style="position:relative;width:18px;height:18px">${S(`left:0px;top:0px;width:18px;height:18px;background-color:#FFFFFF;border:4px solid ${RED};border-radius:9px`)}</div>${T(P(`T${i} – 节点`, {size: 13, lh: 17, color: '#333333', w: 700, align: 'center'}))}</div>`).join('') + `</div>` +
      box('flex:none;height:38px;display:flex;align-items:center', 'background-image:linear-gradient(90deg, #FDE3E6 0%, #FFF4F5 100%);border-radius:6px',
        box('flex:none;width:150px;height:38px;display:flex;align-items:center;justify-content:center;padding-right:12px', `background-color:${RED};clip-path:polygon(0% 0%, 100% 0%, 90% 100%, 0% 100%)`, T(P('»&nbsp;输出标签', {size: 18, lh: 24, color: '#FFFFFF', w: 700, align: 'center'}))) +
        repeat(4, i => (i ? vr('#E9B9BE', 1, true) : '') + G('输出项', 'flex:1;display:flex;align-items:center;justify-content:center;gap:10px', ico(26, {plain: true}) + T(P(`输出成果${zh[i]}`, {size: 15, lh: 20, color: '#222222', w: 700})))).join(''), '输出条') +
      concl('结论句占位：一句话总结本页方案与路线。')
  },
];
