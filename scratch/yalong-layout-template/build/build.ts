import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { syncDesign, convertDesign } from '/home/goodtime/project/pptx-editor-engine/src/design/service';
import { projectPage } from '/home/goodtime/project/pptx-editor-engine/src/design/projection';
import { templateSemanticProposal } from '/home/goodtime/project/pptx-editor-engine/src/design/template-semantics';
import { installNodeEnv } from '/home/goodtime/project/pptx-editor-engine/src/headless/node-env';
import { HeadlessSession } from '/home/goodtime/project/pptx-editor-engine/src/headless/session';
import type { DesignDocument } from '/home/goodtime/project/pptx-editor-engine/src/design/model';
import { layouts, P, tocRows } from './layouts';

installNodeEnv();
const [source, output, renders] = process.argv.slice(2);
mkdirSync(renders, { recursive: true });

const samples: Record<string, string> = {
  TITLE: '智能应用建设方案', SUBTITLE: '阶段成果与后续计划', AUTHOR: '项目组', DATE: '2026 年 11 月',
  ENDING_LABEL: '交流与讨论', ENDING_MESSAGE: '感谢聆听', ENDING_SUBTITLE: '欢迎交流与讨论', ENDING_FOOTNOTE: '项目组',
};
const HEADER_DECOR = ['Rectangle 3', 'Rectangle 5', 'Rectangle 6'];
const TOC_ROW_NODES = ['Rectangle 9', 'TextBox 10', 'Rectangle 11', 'TextBox 12', 'TextBox 13', 'TextBox 14', 'TextBox 15', 'TextBox 16',
  'Rectangle 17', 'TextBox 18', 'Rectangle 19', 'TextBox 20', 'Rectangle 21', 'TextBox 22', 'Rectangle 23', 'TextBox 24'];
const html = (dom: Document) => '<!doctype html>' + dom.documentElement.outerHTML;

/** Template-authoring apply: the host proposal authorizes exactly the purpose changes this HTML makes. */
async function author(design: DesignDocument, index: number, edit: (dom: Document) => void) {
  const before = projectPage(design, design.pages[index].id);
  const dom = new DOMParser().parseFromString(before, 'text/html');
  edit(dom);
  const edited = html(dom);
  const proposal = templateSemanticProposal(design, before, edited);
  const next = await convertDesign({ operation: 'apply', design, expectedRevision: design.revision, html: edited, semanticAuthorization: { ...proposal, mode: 'template-authoring' } } as any);
  const warnings = next.diagnostics.filter(d => d.pageId === next.pages[index].id && d.severity === 'warning');
  console.log(`page ${index + 1}: ${next.pages[index].nodes.length} nodes, ${warnings.length} warnings`);
  for (const w of warnings) console.log('   ', w.message.slice(0, 160));
  return next;
}

let design = await syncDesign(readFileSync(source));
design = await convertDesign({ operation: 'pages', design, expectedRevision: design.revision, commands: layouts.slice(1).map(() => ({ op: 'duplicateSlide' as const, slide: 3 })) });
console.log('pages', design.pages.length);

// Sample wording is wider than the keyword it replaces: widen those boxes about their own centers.
const widen: Record<string, number> = { 'TextBox 6': 760, 'TextBox 7': 600, 'TextBox 11': 300, 'TextBox 12': 300 };
// Brand pages keep their composition: the author marks them layout fill, like the content layouts.
const brandPages: Record<number, {role?: string; layoutName?: string}> = { 0: { role: 'cover', layoutName: '红白品牌封面' }, 1: { role: 'toc', layoutName: '红白品牌目录' } };
for (const index of [0, 1, design.pages.length - 1]) design = await author(design, index, dom => {
  const brand = brandPages[index];
  if (brand?.role) dom.body.dataset.pageRole = brand.role;
  if (brand?.layoutName) dom.body.dataset.layoutName = brand.layoutName;
  dom.body.dataset.pageFill = 'layout';
  if (index === design.pages.length - 1) for (const span of Array.from(dom.querySelectorAll('[data-name="TextBox 13"] span'))) span.textContent = `-${design.pages.length}-`;
  // The TOC's loose tiles, bars and sub-items become repeatable row blocks in the same place and colours.
  if (index === 1) {
    for (const name of TOC_ROW_NODES) dom.querySelector(`[data-name="${name}"]`)!.remove();
    dom.body.insertAdjacentHTML('beforeend', tocRows);
  }
  for (const el of Array.from(dom.querySelectorAll<HTMLElement>('[data-kind="text"]'))) {
    if (!/\{\{[A-Z_0-9]+\}\}/.test(el.innerHTML)) continue;
    el.innerHTML = el.innerHTML.replace(/\{\{([A-Z_0-9]+)\}\}/g, (all, key) => samples[key] ?? all);
    const width = index === 0 ? widen[el.dataset.name!] : undefined;
    const align = el.querySelector<HTMLElement>('p')?.style.textAlign;
    if (width && width > parseFloat(el.style.width)) {
      if (align === 'center') el.style.left = `${parseFloat(el.style.left) + parseFloat(el.style.width) / 2 - width / 2}px`;
      el.style.width = `${width}px`;
    }
  }
});

for (const [i, layout] of layouts.entries()) {
  const index = 2 + i;
  design = await author(design, index, dom => {
    const named = (name: string) => dom.querySelector<HTMLElement>(`[data-name="${name}"]`)!;
    for (const name of HEADER_DECOR) { named(name).dataset.purpose = 'fixed'; named(name).dataset.note = '页眉品牌装饰，保持不变'; }
    const title = named('TextBox 8');
    title.style.width = '700px';
    title.innerHTML = P(`${i + 1}、${layout.title}`, { size: 38, lh: 46, color: '#000000', w: 700 });
    title.dataset.note = '页面标题，按本页内容改写；与右上品牌标识保持距离';
    named('TextBox 9').innerHTML = P(String(index + 1), { size: 16, lh: 19, color: '#777777', align: 'center' });
    dom.body.dataset.pageRole = 'content';
    dom.body.dataset.layoutName = layout.name;
    dom.body.dataset.pageFill = 'layout';
    dom.body.dataset.pageNote = layout.note;
    dom.body.insertAdjacentHTML('beforeend', `<div style="position:absolute;left:22px;top:100px;width:1236px;height:578px;display:flex;flex-direction:column;gap:10px">${layout.html}</div>`);
  });
}

// Second pass: every group the layouts create is a block. The projection lists groups in the same DOM order as the
// layout source, so the source's data-block names annotate them: icon placeholders become reference visuals, the
// other blocks keep the content purpose and say how to vary their count without redrawing them.
const ICON_NOTE = '图标占位：按本块内容生成相应的 SVG 图标替换此占位，保持占位的尺寸与底色样式';
const VARY = '数量按内容增减：用 fill_layout 按条目提交内容，宿主整块复制或删除并重新排列；块内底色、渐变、边框、圆角、阴影原样保留。';
const BLOCKS: Record<string, string> = {
  红头卡片: '红头卡片块：红色渐变标题栏（白色图标与标题）+ 浅粉渐变卡身 + 阴影，卡身里放要点卡等子块',
  要点卡: '要点卡块：白底圆角轻阴影，左侧图标，右侧红色小标题与红方块列表',
  导语条: '导语条块：浅灰渐变条，左侧图标与红色竖线，关键句红色加粗',
  飘带标题: '飘带标题块：居中深红梯形标签 + 渐隐红线',
  结论条: '结论条块：浅粉渐变条，左端红色斜角 » 标签，红色加粗居中文字',
  价值横条: '价值横条块：浅粉渐变横条，若干图标 + 加粗短语，用细竖线分隔',
  红色分节条: '红色分节条块：红色渐变通栏，白色图标与标题',
  能力格: '能力格块：白底小格，图标 + 加粗短语',
  层标签: '层标签块：深红箭头形标签，白色图标与层名',
  能力条: '能力条块：浅粉渐变底条，内放一排能力格',
  过渡箭头: '过渡箭头块：粉色渐变大箭头，红色说明文字',
  图标环节项: '环节项块：白底圆形图标 + 加粗名称，环节之间用红色箭头',
  价值标签: '价值标签块：红色斜角标签，白色标题',
  提问气泡: '提问气泡块：圆形图标 + 粉边白底气泡',
  方向框: '方向框块：浅粉底框，红色 » 方向说明',
  方向项: '方向项块：白底圆形图标 + 红色小标题 + 说明',
  概念标签: '概念标签块：粉边白底标签，图标 + 红色概念名',
  分支胶囊: '分支胶囊块：粉边白底胶囊，图标 + 名称',
  关系链框: '关系链框块：粉边白底框，内放环节项与分支胶囊',
  时间轴节点: '时间轴节点块：红圈节点 + 时间文字，节点沿红色渐变时间线排列',
  输出条: '输出条块：浅粉渐变条，左端红色 » 标签，若干图标 + 输出成果',
  图标场景项: '场景项块：白底圆形图标 + 加粗名称，下方淡粉弧形底',
  价值项: '价值项块：图标 + 加粗短语，项之间细竖线分隔',
  架构层: '架构层块：左侧深红箭头层标签 + 右侧一排能力格',
  输出项: '输出项块：图标 + 输出成果名称，项之间细竖线分隔',
  目录项: '目录项块：左侧编号方块（num）+ 右侧标题栏（title），第一项红色突出、其余灰色；条目与正文章节一一对应，不加没有对应页面的条目',
};
/** Template slots from the layout source: block groups become "<块名>#<set>", one set per container and block
 * name; separators between repeated items become "sep#<set>"; texts and icons inside a block become its fields. */
function sourceSlots(html: string): { groups: string[]; texts: string[]; shapes: string[] } {
  const src = new DOMParser().parseFromString(html, 'text/html');
  const kind = (el: Element) => el.getAttribute('data-kind');
  const block = (el: Element) => kind(el) === 'group' && !!el.getAttribute('data-block') && el.getAttribute('data-block') !== '图标占位';
  const owner = (el: Element) => { for (let p = el.parentElement; p; p = p.parentElement) if (block(p)) return p; return null; };
  const sets = new Map<string, number>(), parents: Element[] = [];
  const setOf = (el: Element) => {
    let parent = parents.indexOf(el.parentElement!);
    if (parent < 0) parent = parents.push(el.parentElement!) - 1;
    const key = `${parent}:${el.getAttribute('data-block')}`;
    if (!sets.has(key)) sets.set(key, sets.size + 1);
    return sets.get(key)!;
  };
  const all = Array.from(src.querySelectorAll('[data-kind]'));
  for (const el of all) if (block(el)) setOf(el);
  /** A separator sits between the blocks of the set whose container is its nearest ancestor holding blocks. */
  const sepSet = (el: Element) => {
    for (let p = el.parentElement; p; p = p.parentElement) {
      const first = Array.from(p.children).find(block);
      if (first) return setOf(first);
    }
    throw new Error('separator outside any set');
  };
  const fields = new Map<Element, string>();
  for (const b of [null, ...all.filter(block)]) {
    const texts = all.filter(el => kind(el) === 'text' && !el.hasAttribute('data-sep') && !el.hasAttribute('data-deco') && owner(el) === b);
    const icons = all.filter(el => el.getAttribute('data-block') === '图标占位' && owner(el) === b);
    if (!b) continue;
    const names = texts.length === 1 ? ['text'] : texts.map((_, k) => ['title', 'body'][k] ?? `text${k + 1}`);
    texts.forEach((el, k) => fields.set(el, el.getAttribute('data-field') ?? names[k]));
    icons.forEach((el, k) => fields.set(el, k ? `icon${k + 1}` : 'icon'));
  }
  const slot = (el: Element) => block(el) ? `${el.getAttribute('data-block')}#${setOf(el)}` : el.hasAttribute('data-sep') ? `sep#${sepSet(el)}` : fields.get(el) ?? '';
  return {
    groups: all.filter(el => kind(el) === 'group').map(slot),
    texts: all.filter(el => kind(el) === 'text').map(slot),
    shapes: all.filter(el => kind(el) === 'shape').map(slot),
  };
}

// Second pass: annotate what the layouts created. Projection lists the new nodes in the layout source's DOM order,
// so each kind's sequence lines up with the source: icon placeholders become reference visuals, blocks keep the
// content purpose with a note, and every block, field and separator gets its slot for fill_layout.
const annotate = (index: number, name: string, source: string) => author(design, index, dom => {
  const slots = sourceSlots(source);
  const created = (k: string) => Array.from(dom.querySelectorAll<HTMLElement>(`[data-kind="${k}"]`)).filter(el => el.dataset.name?.startsWith('new:'));
  const groups = created('group'), texts = created('text'), shapes = created('shape');
  for (const [kind, nodes, wanted] of [['groups', groups, slots.groups], ['texts', texts, slots.texts], ['shapes', shapes, slots.shapes]] as const)
    if (nodes.length !== wanted.length) throw new Error(`${name}: ${nodes.length} ${kind}, layout source has ${wanted.length}`);
  groups.forEach((group, k) => {
    const slot = slots.groups[k];
    if (slot === 'icon' || slot.startsWith('icon')) {
      for (const el of [group, ...Array.from(group.querySelectorAll<HTMLElement>('[data-kind]'))]) el.dataset.purpose = 'reference';
      group.dataset.note = ICON_NOTE;
    } else if (slot.includes('#')) group.dataset.note = `${BLOCKS[slot.replace(/#\d+$/, '')] ?? slot}。${VARY}`;
    if (slot) group.dataset.slot = slot;
  });
  texts.forEach((el, k) => { if (slots.texts[k]) el.dataset.slot = slots.texts[k]; });
  shapes.forEach((el, k) => { if (slots.shapes[k]) el.dataset.slot = slots.shapes[k]; });
});
design = await annotate(1, '目录', tocRows);
for (const [i, layout] of layouts.entries()) design = await annotate(2 + i, layout.name, layout.html);

const bytes = Buffer.from(design.source.pptxBase64, 'base64');
writeFileSync(output, bytes);
const reopened = await syncDesign(bytes);
const leftovers = reopened.pages.flatMap((p, i) => projectPage(reopened, p.id).match(/\{\{[A-Z_0-9]+\}\}/g)?.map(t => `${i + 1}:${t}`) ?? []);
console.log('reopened pages', reopened.pages.length, 'leftover keywords', leftovers);
for (const [i, page] of reopened.pages.entries()) {
  const kinds = page.nodes.reduce<Record<string, number>>((m, n) => (m[n.kind] = (m[n.kind] ?? 0) + 1, m), {});
  const purposes = page.nodes.reduce<Record<string, number>>((m, n) => (m[n.purpose ?? '-'] = (m[n.purpose ?? '-'] ?? 0) + 1, m), {});
  console.log(`  ${i + 1} ${page.semantics?.layoutName ?? ''}`, JSON.stringify(kinds), JSON.stringify(purposes));
}
const session = await HeadlessSession.open(bytes, { quiet: true });
try { for (let i = 1; i <= reopened.pages.length; i++) writeFileSync(join(renders, `r${String(i).padStart(2, '0')}.png`), await session.render(i, { width: 1280 })); }
finally { session.dispose(); }
console.log('rendered', reopened.pages.length);
