/**
 * Tauri IPC 封装 + 与 Rust 命令类型镜像。
 * 浏览器(vite dev)下自动降级 mock, 便于独立调试 UI。
 */

import { invoke } from '@tauri-apps/api/core'
import { open, save } from '@tauri-apps/plugin-dialog'

// ---------------- 基础类型 ----------------

export interface HealthInfo {
  app_version: string
  db_path: string | null
  db_ready: boolean
  db_version: number
  message: string
}

export interface RecordFilter {
  from?: string
  to?: string
  provider_code?: string
  model_name?: string
  project?: string
  tag?: string
  session_keyword?: string
}

export interface UsageRecord {
  id: number
  recorded_at: string
  source: string
  provider_code: string | null
  model_name: string | null
  session_id: string | null
  request_id: string | null
  prompt_tokens: number | null
  completion_tokens: number | null
  cached_tokens: number | null
  total_tokens: number | null
  cost_usd: number | null
  cost_source: string | null
  project: string | null
  tags: string | null
  note: string | null
}

export interface NewRecord {
  recorded_at: string
  source: string
  provider_code?: string | null
  model_name?: string | null
  session_id?: string | null
  request_id?: string | null
  prompt_tokens?: number | null
  completion_tokens?: number | null
  cached_tokens?: number | null
  cost_usd?: number | null
  cost_source?: string | null
  project?: string | null
  tags?: string[] | null
  note?: string | null
}

export interface RecordPatch {
  recorded_at?: string
  provider_code?: string | null
  model_name?: string | null
  session_id?: string | null
  project?: string | null
  tags?: string[] | null
  note?: string | null
  prompt_tokens?: number | null
  completion_tokens?: number | null
  cached_tokens?: number | null
  cost_usd?: number | null
  recompute_cost: boolean
}

export interface PageResult {
  rows: UsageRecord[]
  total: number
  page: number
  page_size: number
}

export interface Overview {
  record_count: number
  prompt_tokens: number
  completion_tokens: number
  cached_tokens: number
  total_tokens: number
  cost_usd: number | null
  day_count: number
  model_count: number
  provider_count: number
}

export interface TrendPoint {
  day: string
  prompt_tokens: number
  completion_tokens: number
  cached_tokens: number
  total_tokens: number
  cost_usd: number | null
  record_count: number
}

export interface HourTrendPoint {
  hour: string // 2026-09-08T14
  total_tokens: number
  cost_usd: number | null
  record_count: number
}

export interface DistBucket {
  key: string
  total_tokens: number
  cost_usd: number | null
  record_count: number
}

export interface DashboardData {
  overview: Overview
  trend: TrendPoint[]
  by_model: DistBucket[]
  by_provider: DistBucket[]
  by_project: DistBucket[]
  known_models: string[]
}

export interface ModelPrice {
  model_id: number
  provider_code: string
  provider_name: string
  model_name: string
  input_per_mtok: number
  output_per_mtok: number
  cached_input_per_mtok: number | null
  source: string
}

export interface SettingsView {
  display_currency: string
  usd_cny_rate: number
}

export interface CsvPreview {
  headers: string[]
  sample_rows: string[][]
  total_preview_rows: number
}

export interface ImportResult {
  total_rows: number
  ok_rows: number
  failed_rows: number
  skipped_rows: number
  batch_id: number | null
  errors: string[]
}

export interface ColumnMapping {
  map: Record<string, string>
  batch_source?: string | null
}

export interface ExportPayload {
  file_name: string
  content: string
  rows: number
}

// ---------------- 环境判断与调用底座 ----------------

export const inTauri = (): boolean =>
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

/** invoke 的浏览器降级包装: browser 模式直接返回 mock 值 */
async function call<R>(cmd: string, args: unknown, mock: () => R): Promise<R> {
  if (!inTauri()) {
    console.info(`[mock] ${cmd}`, args)
    return mock()
  }
  return invoke<R>(cmd, args as never)
}

// ---------------- 命令封装 ----------------

export const getHealth = (): Promise<HealthInfo> =>
  call('health', {}, () => ({
    app_version: '0.1.0 (browser preview)',
    db_path: null,
    db_ready: false,
    db_version: 0,
    message: 'Browser preview: Tauri backend not connected',
  }))

export interface CollectorStatusInfo {
  port: number
  started: boolean
  error: string | null
  base_url: string
}

export const collectorStatus = (): Promise<CollectorStatusInfo> =>
  call('collector_status', {}, () => ({
    port: 8765,
    started: false,
    error: null,
    base_url: 'http://127.0.0.1:8765',
  }))

export const listRecords = (
  filter: RecordFilter,
  page: number,
  pageSize: number,
): Promise<PageResult> =>
  call(
    'list_records',
    { q: { filter, page, page_size: pageSize } },
    () => ({ rows: [], total: 0, page, page_size: pageSize }),
  )

export const addRecord = (rec: NewRecord): Promise<number> =>
  call('add_record', { rec }, () => 1)

export const updateRecord = (id: number, patch: RecordPatch): Promise<boolean> =>
  call('update_record', { id, patch }, () => true)

export const deleteRecord = (id: number): Promise<boolean> =>
  call('delete_record', { id }, () => true)

export const getDashboard = (filter: RecordFilter): Promise<DashboardData> =>
  call('dashboard', { filter }, () => emptyDashboard())

export const getHourlyTrend = (filter: RecordFilter): Promise<HourTrendPoint[]> =>
  call('usage_hourly_trend', { filter }, () => [])

const emptyDashboard = (): DashboardData => ({
  overview: {
    record_count: 0,
    prompt_tokens: 0,
    completion_tokens: 0,
    cached_tokens: 0,
    total_tokens: 0,
    cost_usd: null,
    day_count: 0,
    model_count: 0,
    provider_count: 0,
  },
  trend: [],
  by_model: [],
  by_provider: [],
  by_project: [],
  known_models: [],
})

export const listPrices = (): Promise<ModelPrice[]> =>
  call('list_prices', {}, () => [])

export const upsertCustomPrice = (p: {
  provider_code: string
  provider_name: string
  model_name: string
  input_per_mtok: number
  output_per_mtok: number
  cached_input_per_mtok?: number | null
}): Promise<number> => call('upsert_custom_price', { p }, () => 1)

export const getSettings = (): Promise<SettingsView> =>
  call('get_settings', {}, () => ({ display_currency: 'CNY', usd_cny_rate: 7.1 }))

export const setSettings = (s: SettingsView): Promise<void> =>
  call('set_settings', { s }, () => undefined)

export const previewCsv = (path: string, limit = 15): Promise<CsvPreview> =>
  call('preview_csv', { path, limit }, () => ({
    headers: [],
    sample_rows: [],
    total_preview_rows: 0,
  }))

export const importCsv = (
  path: string,
  mapping: ColumnMapping,
  file_name: string,
): Promise<ImportResult> =>
  call('import_csv', { path, mapping, file_name }, () => ({
    total_rows: 0,
    ok_rows: 0,
    failed_rows: 0,
    skipped_rows: 0,
    batch_id: null,
    errors: [],
  }))

export const exportData = (
  filter: RecordFilter,
  format: 'csv' | 'json',
): Promise<ExportPayload> =>
  call('export_data', { q: { filter, format } }, () => ({
    file_name: `export.${format}`,
    content: '',
    rows: 0,
  }))

export const writeTextFile = (path: string, content: string): Promise<void> =>
  call('write_text_file', { path, content }, () => undefined)

export const backupDb = (dest: string): Promise<void> =>
  call('backup_db', { dest_path: dest }, () => undefined)

export const restoreDb = (src: string): Promise<number> =>
  call('restore_db', { src_path: src }, () => 0)

// ---------------- 文件对话框封装 ----------------

export async function pickImportFile(): Promise<string | null> {
  if (!inTauri()) {
    throw new Error('浏览器预览模式不支持选择本地文件，请运行 tauri dev')
  }
  const picked = await open({
    multiple: false,
    directory: false,
    filters: [
      { name: '数据文件', extensions: ['csv', 'json', 'ndjson', 'txt'] },
      { name: '所有文件', extensions: ['*'] },
    ],
  })
  return typeof picked === 'string' ? picked : null
}

export async function pickSavePath(
  defaultName: string,
  extensions: string[],
): Promise<string | null> {
  if (!inTauri()) {
    throw new Error('浏览器预览模式不支持保存本地文件，请运行 tauri dev')
  }
  const picked = await save({
    defaultPath: defaultName,
    filters: [
      { name: '导出文件', extensions },
      { name: '所有文件', extensions: ['*'] },
    ],
  })
  return typeof picked === 'string' ? picked : null
}

export async function pickOpenPath(
  extensions: string[],
  label = '选择文件',
): Promise<string | null> {
  if (!inTauri()) {
    throw new Error('浏览器预览模式不支持选择本地文件，请运行 tauri dev')
  }
  const picked = await open({
    multiple: false,
    directory: false,
    filters: [{ name: label, extensions }, { name: '所有文件', extensions: ['*'] }],
  })
  return typeof picked === 'string' ? picked : null
}
