import audioIcon from '../../node_modules/material-icon-theme/icons/audio.svg?url'
import consoleIcon from '../../node_modules/material-icon-theme/icons/console.svg?url'
import cssIcon from '../../node_modules/material-icon-theme/icons/css.svg?url'
import databaseIcon from '../../node_modules/material-icon-theme/icons/database.svg?url'
import documentIcon from '../../node_modules/material-icon-theme/icons/document.svg?url'
import fileIcon from '../../node_modules/material-icon-theme/icons/file.svg?url'
import goIcon from '../../node_modules/material-icon-theme/icons/go.svg?url'
import htmlIcon from '../../node_modules/material-icon-theme/icons/html.svg?url'
import imageIcon from '../../node_modules/material-icon-theme/icons/image.svg?url'
import javaIcon from '../../node_modules/material-icon-theme/icons/java.svg?url'
import javascriptIcon from '../../node_modules/material-icon-theme/icons/javascript.svg?url'
import jsonIcon from '../../node_modules/material-icon-theme/icons/json.svg?url'
import markdownIcon from '../../node_modules/material-icon-theme/icons/markdown.svg?url'
import pdfIcon from '../../node_modules/material-icon-theme/icons/pdf.svg?url'
import powerpointIcon from '../../node_modules/material-icon-theme/icons/powerpoint.svg?url'
import pythonIcon from '../../node_modules/material-icon-theme/icons/python.svg?url'
import reactIcon from '../../node_modules/material-icon-theme/icons/react.svg?url'
import reactTsIcon from '../../node_modules/material-icon-theme/icons/react_ts.svg?url'
import rustIcon from '../../node_modules/material-icon-theme/icons/rust.svg?url'
import tableIcon from '../../node_modules/material-icon-theme/icons/table.svg?url'
import typescriptIcon from '../../node_modules/material-icon-theme/icons/typescript.svg?url'
import videoIcon from '../../node_modules/material-icon-theme/icons/video.svg?url'
import wordIcon from '../../node_modules/material-icon-theme/icons/word.svg?url'
import xmlIcon from '../../node_modules/material-icon-theme/icons/xml.svg?url'
import yamlIcon from '../../node_modules/material-icon-theme/icons/yaml.svg?url'
import zipIcon from '../../node_modules/material-icon-theme/icons/zip.svg?url'
import type { StudioModuleIcon } from '../officeStudio.ts'

export type FileVisualTone =
  | 'pdf'
  | 'word'
  | 'excel'
  | 'ppt'
  | 'markdown'
  | 'database'
  | 'image'
  | 'code'
  | 'archive'
  | 'media'
  | 'default'

export interface FileVisualMeta {
  readonly iconUrl: string
  readonly label: string
  readonly ext: string
  readonly tone: FileVisualTone
}

const STUDIO_ICON_URLS: Record<StudioModuleIcon, string> = {
  ppt: powerpointIcon,
  word: wordIcon,
  excel: tableIcon,
  pdf: pdfIcon,
  markdown: markdownIcon,
  database: databaseIcon,
  app: documentIcon,
}

const EXT_META: Record<string, { iconUrl: string; label: string; tone: FileVisualTone }> = {
  pdf: { iconUrl: pdfIcon, label: 'PDF 文档', tone: 'pdf' },
  doc: { iconUrl: wordIcon, label: 'Word 文档', tone: 'word' },
  docx: { iconUrl: wordIcon, label: 'Word 文档', tone: 'word' },
  rtf: { iconUrl: wordIcon, label: '富文本文档', tone: 'word' },
  xls: { iconUrl: tableIcon, label: 'Excel 表格', tone: 'excel' },
  xlsx: { iconUrl: tableIcon, label: 'Excel 表格', tone: 'excel' },
  csv: { iconUrl: tableIcon, label: 'CSV 表格', tone: 'excel' },
  tsv: { iconUrl: tableIcon, label: 'TSV 表格', tone: 'excel' },
  ppt: { iconUrl: powerpointIcon, label: 'PPT 演示', tone: 'ppt' },
  pptx: { iconUrl: powerpointIcon, label: 'PPT 演示', tone: 'ppt' },
  md: { iconUrl: markdownIcon, label: 'Markdown', tone: 'markdown' },
  markdown: { iconUrl: markdownIcon, label: 'Markdown', tone: 'markdown' },
  mdx: { iconUrl: markdownIcon, label: 'MDX 文档', tone: 'markdown' },
  sql: { iconUrl: databaseIcon, label: 'SQL 脚本', tone: 'database' },
  db: { iconUrl: databaseIcon, label: '数据库', tone: 'database' },
  sqlite: { iconUrl: databaseIcon, label: 'SQLite', tone: 'database' },
  sqlite3: { iconUrl: databaseIcon, label: 'SQLite', tone: 'database' },
  png: { iconUrl: imageIcon, label: 'PNG 图片', tone: 'image' },
  jpg: { iconUrl: imageIcon, label: 'JPG 图片', tone: 'image' },
  jpeg: { iconUrl: imageIcon, label: 'JPEG 图片', tone: 'image' },
  webp: { iconUrl: imageIcon, label: 'WebP 图片', tone: 'image' },
  gif: { iconUrl: imageIcon, label: 'GIF 动图', tone: 'image' },
  svg: { iconUrl: imageIcon, label: 'SVG 矢量图', tone: 'image' },
  bmp: { iconUrl: imageIcon, label: 'BMP 图片', tone: 'image' },
  ico: { iconUrl: imageIcon, label: 'ICO 图标', tone: 'image' },
  zip: { iconUrl: zipIcon, label: 'ZIP 压缩包', tone: 'archive' },
  tar: { iconUrl: zipIcon, label: 'TAR 归档', tone: 'archive' },
  gz: { iconUrl: zipIcon, label: 'GZ 压缩包', tone: 'archive' },
  rar: { iconUrl: zipIcon, label: 'RAR 压缩包', tone: 'archive' },
  '7z': { iconUrl: zipIcon, label: '7Z 压缩包', tone: 'archive' },
  mp3: { iconUrl: audioIcon, label: '音频文件', tone: 'media' },
  wav: { iconUrl: audioIcon, label: '音频文件', tone: 'media' },
  ogg: { iconUrl: audioIcon, label: '音频文件', tone: 'media' },
  m4a: { iconUrl: audioIcon, label: '音频文件', tone: 'media' },
  flac: { iconUrl: audioIcon, label: '音频文件', tone: 'media' },
  mp4: { iconUrl: videoIcon, label: '视频文件', tone: 'media' },
  mov: { iconUrl: videoIcon, label: '视频文件', tone: 'media' },
  webm: { iconUrl: videoIcon, label: '视频文件', tone: 'media' },
  mkv: { iconUrl: videoIcon, label: '视频文件', tone: 'media' },
  ts: { iconUrl: typescriptIcon, label: 'TypeScript', tone: 'code' },
  mts: { iconUrl: typescriptIcon, label: 'TypeScript', tone: 'code' },
  cts: { iconUrl: typescriptIcon, label: 'TypeScript', tone: 'code' },
  tsx: { iconUrl: reactTsIcon, label: 'TSX 组件', tone: 'code' },
  js: { iconUrl: javascriptIcon, label: 'JavaScript', tone: 'code' },
  mjs: { iconUrl: javascriptIcon, label: 'JavaScript', tone: 'code' },
  cjs: { iconUrl: javascriptIcon, label: 'JavaScript', tone: 'code' },
  jsx: { iconUrl: reactIcon, label: 'JSX 组件', tone: 'code' },
  java: { iconUrl: javaIcon, label: 'Java 代码', tone: 'code' },
  py: { iconUrl: pythonIcon, label: 'Python 代码', tone: 'code' },
  rs: { iconUrl: rustIcon, label: 'Rust 代码', tone: 'code' },
  go: { iconUrl: goIcon, label: 'Go 代码', tone: 'code' },
  json: { iconUrl: jsonIcon, label: 'JSON 数据', tone: 'code' },
  jsonc: { iconUrl: jsonIcon, label: 'JSON 配置', tone: 'code' },
  jsonl: { iconUrl: jsonIcon, label: 'JSONL 数据', tone: 'code' },
  yaml: { iconUrl: yamlIcon, label: 'YAML 配置', tone: 'code' },
  yml: { iconUrl: yamlIcon, label: 'YAML 配置', tone: 'code' },
  html: { iconUrl: htmlIcon, label: 'HTML 页面', tone: 'code' },
  htm: { iconUrl: htmlIcon, label: 'HTML 页面', tone: 'code' },
  css: { iconUrl: cssIcon, label: 'CSS 样式', tone: 'code' },
  scss: { iconUrl: cssIcon, label: 'SCSS 样式', tone: 'code' },
  less: { iconUrl: cssIcon, label: 'LESS 样式', tone: 'code' },
  xml: { iconUrl: xmlIcon, label: 'XML 文档', tone: 'code' },
  sh: { iconUrl: consoleIcon, label: 'Shell 脚本', tone: 'code' },
  bash: { iconUrl: consoleIcon, label: 'Shell 脚本', tone: 'code' },
  zsh: { iconUrl: consoleIcon, label: 'Shell 脚本', tone: 'code' },
  ps1: { iconUrl: consoleIcon, label: 'PowerShell', tone: 'code' },
  bat: { iconUrl: consoleIcon, label: '批处理脚本', tone: 'code' },
  txt: { iconUrl: documentIcon, label: '纯文本', tone: 'default' },
  log: { iconUrl: documentIcon, label: '日志文件', tone: 'default' },
}

export function fileVisualMeta(pathOrName: string): FileVisualMeta {
  const base = pathOrName.split(/[\\/]/u).pop() ?? pathOrName
  const dot = base.lastIndexOf('.')
  const ext = dot > 0 ? base.slice(dot + 1).toLowerCase() : ''
  const hit = ext ? EXT_META[ext] : undefined
  if (hit) {
    return { ...hit, ext: ext.toUpperCase() }
  }
  return {
    iconUrl: ext ? fileIcon : documentIcon,
    label: ext ? `${ext.toUpperCase()} 文件` : '附件文件',
    ext: ext ? ext.toUpperCase() : 'FILE',
    tone: 'default',
  }
}

export function formatFileSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return ''
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes >= 100 * 1024 ? 0 : 1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

export function RichFileIcon({
  path,
  size = 22,
  className,
}: {
  readonly path: string
  readonly size?: number | undefined
  readonly className?: string | undefined
}) {
  const meta = fileVisualMeta(path)
  return (
    <img
      src={meta.iconUrl}
      width={size}
      height={size}
      alt=""
      aria-hidden="true"
      draggable={false}
      className={className}
      style={{ display: 'block', width: size, height: size, flex: 'none', objectFit: 'contain' }}
    />
  )
}

export function StudioModuleIconImg({
  kind,
  size = 24,
  className,
}: {
  readonly kind: StudioModuleIcon
  readonly size?: number | undefined
  readonly className?: string | undefined
}) {
  const url = STUDIO_ICON_URLS[kind] ?? documentIcon
  return (
    <img
      src={url}
      width={size}
      height={size}
      alt=""
      aria-hidden="true"
      draggable={false}
      className={className}
      style={{ display: 'block', width: size, height: size, flex: 'none', objectFit: 'contain' }}
    />
  )
}
