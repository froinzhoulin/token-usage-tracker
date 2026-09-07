import { useState } from 'react'

import {
  importCsv,
  pickImportFile,
  previewCsv,
  type ColumnMapping,
  type CsvPreview,
  type ImportResult,
} from '../api/client'

/** 目标字段及其显示名 */
const TARGET_FIELDS: { key: string; label: string; required?: boolean; hint?: string }[] = [
  { key: 'recorded_at', label: '时间(recorded_at)', required: true, hint: '2026-01-01 或 2026-01-01 08:00:00' },
  { key: 'model_name', label: '模型(model_name)', required: true },
  { key: 'provider_code', label: '厂商(provider_code)', hint: 'deepseek/openai/anthropic/自定义' },
  { key: 'prompt_tokens', label: '输入Token' },
  { key: 'completion_tokens', label: '输出Token' },
  { key: 'cached_tokens', label: '缓存命中Token' },
  { key: 'cost_usd', label: '费用USD(cost_usd)', hint: '有则优先采用' },
  { key: 'session_id', label: '会话ID' },
  { key: 'request_id', label: '请求ID', hint: '用于去重' },
  { key: 'project', label: '项目' },
  { key: 'tags', label: '标签(tags)' },
  { key: 'note', label: '备注(note)' },
]

export default function ImportPage() {
  const [path, setPath] = useState<string | null>(null)
  const [preview, setPreview] = useState<CsvPreview | null>(null)
  const [map, setMap] = useState<Record<string, string>>({})
  const [batchSource, setBatchSource] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<ImportResult | null>(null)

  async function handlePick() {
    setError(null)
    setResult(null)
    try {
      const p = await pickImportFile()
      if (!p) return
      setPath(p)
      setMap({})
      const prev = await previewCsv(p, 10)
      setPreview(prev)
      // 尝试按常见列名自动预映射
      const auto: Record<string, string> = {}
      for (const h of prev.headers) {
        const key = guessField(h)
        if (key) auto[h] = key
      }
      setMap(auto)
    } catch (e) {
      setError(String(e))
    }
  }

  async function handleImport() {
    if (!path) return
    setBusy(true)
    setError(null)
    setResult(null)
    try {
      const mapping: ColumnMapping = {
        map,
        batch_source: batchSource || null,
      }
      const fileName = path.split(/[\\/]/).pop() ?? path
      const res = await importCsv(path, mapping, fileName)
      setResult(res)
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="page">
      <div className="page-header">
        <h2>导入用量记录</h2>
      </div>

      <div className="panel">
        <h3>第 1 步：选择文件</h3>
        <p className="muted">
          支持 CSV / NDJSON / TXT（当前向导按 CSV 处理，JSON 模板预留在后续版本）。可自定义字段映射以适配不同来源的导出格式。
        </p>
        <div className="row">
          <button className="btn primary" onClick={handlePick}>
            选择 CSV 文件…
          </button>
          {path && <span className="muted ellipsis">{path}</span>}
        </div>
      </div>

      {error && <div className="error-box">{error}</div>}

      {preview && (
        <>
          <div className="panel">
            <h3>第 2 步：字段映射</h3>
            <p className="muted">源文件表头：{preview.headers.join('、') || '（无）'}</p>
            <table className="map-table">
              <thead>
                <tr>
                  <th>源列</th>
                  <th>样例值</th>
                  <th>映射到</th>
                </tr>
              </thead>
              <tbody>
                {preview.headers.map((h) => (
                  <tr key={h}>
                    <td>
                      <strong>{h}</strong>
                      {TARGET_FIELDS.find((t) => t.key === map[h])?.required && (
                        <span className="req">*必填</span>
                      )}
                    </td>
                    <td className="ellipsis">{preview.sample_rows[0]?.[preview.headers.indexOf(h)] ?? ''}</td>
                    <td>
                      <select
                        value={map[h] ?? ''}
                        onChange={(e) => setMap({ ...map, [h]: e.target.value })}
                      >
                        <option value="">— 不导入 —</option>
                        {TARGET_FIELDS.map((t) => (
                          <option key={t.key} value={t.key}>
                            {t.label}
                          </option>
                        ))}
                      </select>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>

            <div className="row" style={{ marginTop: 12 }}>
              <input
                placeholder="批次备注(写入 note，可选)"
                value={batchSource}
                onChange={(e) => setBatchSource(e.target.value)}
              />
              <button className="btn primary" disabled={busy} onClick={handleImport}>
                {busy ? '导入中…' : '开始导入'}
              </button>
            </div>
          </div>
        </>
      )}

      {result && (
        <div className={`panel result ${result.failed_rows > 0 ? 'warn-result' : ''}`}>
          <h3>导入结果</h3>
          <p>
            总行数 <strong>{result.total_rows}</strong> · 成功{' '}
            <strong className="ok-text">{result.ok_rows}</strong> · 跳过(重复){' '}
            {result.skipped_rows} · 失败 <strong>{result.failed_rows}</strong>
          </p>
          {result.errors.length > 0 && (
            <details>
              <summary>查看错误明细（{result.errors.length} 条）</summary>
              <ul className="err-list">
                {result.errors.map((e, i) => (
                  <li key={i}>{e}</li>
                ))}
              </ul>
            </details>
          )}
        </div>
      )}
    </div>
  )
}

/** 依据常见列名猜测目标字段 */
function guessField(header: string): string {
  const h = header.trim().toLowerCase()
  const rules: [RegExp, string][] = [
    [/^(timestamp|time|date|recorded_at|时间|日期|created)/, 'recorded_at'],
    [/^(model|model_name|模型)/, 'model_name'],
    [/^(provider|vendor|厂商)/, 'provider_code'],
    [/^(input|prompt).*(token)|prompt_tokens|输入/, 'prompt_tokens'],
    [/^(output|completion).*(token)|completion_tokens|输出/, 'completion_tokens'],
    [/cached|缓存/, 'cached_tokens'],
    [/^(cost|price|费用|金额|total_cost)/, 'cost_usd'],
    [/session|会话/, 'session_id'],
    [/request|请求.*id|req_id/, 'request_id'],
    [/^project|项目/, 'project'],
    [/^tag|标签/, 'tags'],
    [/^note|备注|说明/, 'note'],
  ]
  for (const [re, field] of rules) {
    if (re.test(h)) return field
  }
  return ''
}
