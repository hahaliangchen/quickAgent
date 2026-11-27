import type { SkillPackage } from './api.ts'

export type StudioModuleIcon = 'ppt' | 'word' | 'markdown' | 'excel' | 'pdf' | 'database' | 'app'

export interface StudioModuleCard {
  readonly key: string
  readonly slotId: string
  readonly owner?: string
  readonly page?: string
  readonly title: string
  readonly subtitle: string
  readonly formats: readonly string[]
  readonly icon: StudioModuleIcon
  readonly accent: 'ppt' | 'word' | 'markdown' | 'excel' | 'pdf' | 'database' | 'default'
  readonly ready: boolean
  readonly bundleDir: string
  readonly toolCount: number
}

interface SuiteSlotTemplate {
  readonly slotId: string
  readonly matchIds: readonly string[]
  readonly title: string
  readonly subtitle: string
  readonly formats: readonly string[]
  readonly icon: StudioModuleIcon
  readonly accent: StudioModuleCard['accent']
  readonly bundleDir: string
}

export const OFFICE_SUITE_SLOTS: readonly SuiteSlotTemplate[] = [
  {
    slotId: 'ppt',
    matchIds: ['ppt', 'ppt-editor', 'pptx', 'ppt-master'],
    title: 'PPT 演示文稿',
    subtitle: 'Rust WASM 高精度幻灯片渲染、可视化编辑与 .pptx 导出工作台。',
    formats: ['.pptx'],
    icon: 'ppt',
    accent: 'ppt',
    bundleDir: 'bundled_plugins/ppt/',
  },
  {
    slotId: 'word',
    matchIds: ['word', 'word-editor', 'docx', 'docx-editor'],
    title: 'Word 文档',
    subtitle: 'OOXML 无损读写、A4 纸张流式编辑、大纲导航与结构化文档排版。',
    formats: ['.docx'],
    icon: 'word',
    accent: 'word',
    bundleDir: 'bundled_plugins/word/',
  },
  {
    slotId: 'markdown',
    matchIds: ['markdown', 'md', 'md-editor', 'markdown-editor'],
    title: 'Markdown 编辑器',
    subtitle: '基于通用编辑器内核的轻量双栏写作台，支持实时预览、公式与流程图。',
    formats: ['.md'],
    icon: 'markdown',
    accent: 'markdown',
    bundleDir: 'bundled_plugins/markdown/',
  },
  {
    slotId: 'excel',
    matchIds: ['excel', 'sheet', 'sheets', 'xlsx', 'excel-editor'],
    title: 'Excel 表格',
    subtitle: '多工作表电子表格浏览、公式计算、选区分析与 .xlsx 导出。',
    formats: ['.xlsx'],
    icon: 'excel',
    accent: 'excel',
    bundleDir: 'bundled_plugins/excel/',
  },
  {
    slotId: 'pdf',
    matchIds: ['pdf', 'pdf-viewer', 'pdf-editor', 'pdf-reader'],
    title: 'PDF 阅读与批注',
    subtitle: '高精度 PDF 预览、大纲跳转、高亮批注、页面合并拆分与文本/表格提取。',
    formats: ['.pdf'],
    icon: 'pdf',
    accent: 'pdf',
    bundleDir: 'bundled_plugins/pdf/',
  },
]

function inferIconAndAccent(id: string): { icon: StudioModuleIcon; accent: StudioModuleCard['accent'] } {
  const lower = id.toLowerCase()
  if (lower.includes('ppt') || lower.includes('slide')) return { icon: 'ppt', accent: 'ppt' }
  if (lower.includes('word') || lower.includes('doc')) return { icon: 'word', accent: 'word' }
  if (lower.includes('markdown') || lower === 'md' || lower.startsWith('md-')) return { icon: 'markdown', accent: 'markdown' }
  if (lower.includes('excel') || lower.includes('sheet') || lower.includes('xlsx')) return { icon: 'excel', accent: 'excel' }
  if (lower.includes('pdf')) return { icon: 'pdf', accent: 'pdf' }
  if (lower.includes('db') || lower.includes('database') || lower.includes('sql')) return { icon: 'database', accent: 'database' }
  return { icon: 'app', accent: 'default' }
}

/**
 * Merge enabled external/bundled packages that declare pages into the unified Office Studio catalog.
 * Bundled modules automatically light up their corresponding suite card; any additional custom
 * module with pages is appended as an active card so nothing is ever hardcoded out.
 */
export function buildStudioCatalog(packages: readonly SkillPackage[]): StudioModuleCard[] {
  const candidates = packages.filter(
    pkg => pkg.enabled && pkg.origin === 'external' && !pkg.mcp && pkg.pages.length > 0,
  )
  const claimed = new Set<string>()
  const cards: StudioModuleCard[] = []

  for (const slot of OFFICE_SUITE_SLOTS) {
    const matched = candidates.find(pkg => {
      if (claimed.has(pkg.id)) return false
      const id = pkg.id.toLowerCase()
      return slot.matchIds.includes(id) || id.startsWith(`${slot.slotId}-`)
    })
    if (matched) {
      claimed.add(matched.id)
      const primary = matched.pages[0]!
      cards.push({
        key: `${matched.id}:${primary.id}`,
        slotId: slot.slotId,
        owner: matched.id,
        page: primary.id,
        title: primary.title || matched.title || slot.title,
        subtitle: matched.description || slot.subtitle,
        formats: slot.formats,
        icon: slot.icon,
        accent: slot.accent,
        ready: true,
        bundleDir: slot.bundleDir,
        toolCount: matched.tools.length,
      })
    } else {
      cards.push({
        key: `slot:${slot.slotId}`,
        slotId: slot.slotId,
        title: slot.title,
        subtitle: slot.subtitle,
        formats: slot.formats,
        icon: slot.icon,
        accent: slot.accent,
        ready: false,
        bundleDir: slot.bundleDir,
        toolCount: 0,
      })
    }
  }

  for (const pkg of candidates) {
    if (claimed.has(pkg.id)) continue
    const primary = pkg.pages[0]!
    const { icon, accent } = inferIconAndAccent(pkg.id)
    cards.push({
      key: `${pkg.id}:${primary.id}`,
      slotId: pkg.id,
      owner: pkg.id,
      page: primary.id,
      title: primary.title || pkg.title,
      subtitle: pkg.description || '已装配的独立扩展工作台模块。',
      formats: [],
      icon,
      accent,
      ready: true,
      bundleDir: `bundled_plugins/${pkg.id}/`,
      toolCount: pkg.tools.length,
    })
  }

  return cards
}
