import type { DailyReportSyncAction, DbStudioSyncAction, ExcelStudioSyncAction } from './protocol.ts'

function parseJsonRecord(raw: unknown): Record<string, unknown> | undefined {
  if (raw && typeof raw === 'object' && !Array.isArray(raw)) return raw as Record<string, unknown>
  if (typeof raw !== 'string' || !raw.trim()) return undefined
  try {
    const parsed = JSON.parse(raw)
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed as Record<string, unknown> : undefined
  } catch { return undefined }
}

function asNonEmptyString(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined
}

function asPositiveInt(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) && value >= 1 ? Math.floor(value) : undefined
}

function asNonNegativeInt(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? Math.floor(value) : undefined
}

function asColumnSpec(value: unknown): string | undefined {
  const str = asNonEmptyString(value)
  if (str) return str
  const num = asPositiveInt(value)
  return num !== undefined ? String(num) : undefined
}

export function extractDbSyncAction(
  name: string,
  args: Record<string, unknown>,
  result: Record<string, unknown> | undefined,
): DbStudioSyncAction | undefined {
  const connObj = parseJsonRecord(result?.connection)
  const connectionId = asNonEmptyString(args.connection_id) ?? asNonEmptyString(connObj?.id)
  const sqlitePath = asNonEmptyString(args.sqlite_path) ?? asNonEmptyString(connObj?.file_path)
  if (name === 'db__save_connection') {
    return { action: 'save_connection', connectionId, sqlitePath }
  }
  if (name === 'db__inspect_schema') {
    const schema = asNonEmptyString(args.schema)
      ?? asNonEmptyString(result?.schema)
      ?? asNonEmptyString(result?.default_schema)
    const table = asNonEmptyString(args.table) ?? asNonEmptyString(result?.table)
    return { action: 'inspect', connectionId, sqlitePath, schema, table }
  }
  if (name === 'db__query') {
    // Background analytical / counting / sampling queries stay silent so they never
    // hijack the user's open table tab or re-execute on the database a second time.
    // Only sync to the UI when the agent explicitly opts in via `show_in_ui: true`.
    if (args.show_in_ui !== true) return undefined
    const editable = parseJsonRecord(result?.editable_table)
    return {
      action: 'query',
      connectionId,
      sqlitePath,
      sql: asNonEmptyString(args.sql),
      schema: asNonEmptyString(editable?.schema),
      table: asNonEmptyString(editable?.table),
    }
  }
  if (name === 'db__explain') {
    return { action: 'explain', connectionId, sqlitePath, sql: asNonEmptyString(args.sql) }
  }
  if (name === 'db__execute') {
    return {
      action: 'execute',
      connectionId,
      sqlitePath,
      schema: asNonEmptyString(args.schema),
      table: asNonEmptyString(args.table),
    }
  }
  if (name === 'db__focus_view') {
    return {
      action: 'focus_view',
      connectionId,
      sqlitePath,
      schema: asNonEmptyString(args.schema),
      table: asNonEmptyString(args.table),
      column: asColumnSpec(args.column),
      row: asPositiveInt(args.row),
      offset: asNonNegativeInt(args.offset),
      range: asNonEmptyString(args.range),
      findColumn: asColumnSpec(args.find_column),
      findValue: asNonEmptyString(args.find_value),
    }
  }
  return undefined
}

export function extractExcelSyncAction(
  name: string,
  args: Record<string, unknown>,
): ExcelStudioSyncAction | undefined {
  if (name === 'excel__focus_view') {
    return {
      path: asNonEmptyString(args.path),
      sheet: asNonEmptyString(args.sheet),
      range: asNonEmptyString(args.range),
      column: asColumnSpec(args.column),
      row: asPositiveInt(args.row),
      findColumn: asColumnSpec(args.find_column),
      findValue: asNonEmptyString(args.find_value),
    }
  }
  if (name === 'excel__read_range' || name === 'excel__format_range') {
    const range = asNonEmptyString(args.range)
    if (!range) return undefined
    return { path: asNonEmptyString(args.path), sheet: asNonEmptyString(args.sheet), range }
  }
  if (name === 'excel__search') {
    const query = asNonEmptyString(args.query)
    if (!query) return undefined
    return { path: asNonEmptyString(args.path), sheet: asNonEmptyString(args.sheet), findValue: query }
  }
  return undefined
}

export function extractDailyReportSyncAction(
  name: string,
  args: Record<string, unknown>,
  result: Record<string, unknown> | undefined,
): DailyReportSyncAction | undefined {
  if (name === 'daily_report__prepare_draft') {
    return {
      action: 'prepare_draft',
      reportDate: asNonEmptyString(args.report_date) ?? asNonEmptyString(result?.report_date),
    }
  }
  if (name === 'daily_report__write_guide') {
    return {
      action: 'write_guide',
      content: typeof args.content === 'string' ? args.content : undefined,
    }
  }
  if (name === 'daily_report__submit') {
    return {
      action: 'submit',
      reportDate: asNonEmptyString(result?.report_date),
    }
  }
  if (name === 'daily_report__today_activity') {
    return {
      action: 'today_activity',
      reportDate: asNonEmptyString(args.date) ?? asNonEmptyString(result?.date),
    }
  }
  if (name === 'daily_report__list') {
    return {
      action: 'list',
      reportDate: asNonEmptyString(args.report_date),
    }
  }
  return undefined
}
