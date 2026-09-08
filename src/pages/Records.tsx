import { useCallback, useEffect, useMemo, useState } from 'react'

import {
  deleteRecord,
  getSettings,
  listRecords,
  updateRecord,
  type RecordFilter,
  type SettingsView,
  type UsageRecord,
} from '../api/client'
import { displayCost, fmtDate, fmtTokens } from '../utils/format'

const PAGE_SIZE = 25

/** 行内编辑草稿(与 UsageRecord 展示形态解耦) */
interface EditDraft {
  recorded_at?: string
  provider_code?: string | null
  model_name?: string | null
  session_id?: string | null
  prompt_tokens?: number | null
  completion_tokens?: number | null
  cached_tokens?: number | null
  cost_usd?: number | null
  project?: string | null
  tags?: string[]
  note?: string | null
}

const emptyFilter = (): RecordFilter => ({})

export default function Records() {
  const [rows, setRows] = useState<UsageRecord[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(0)
  const [filter, setFilter] = useState<RecordFilter>(emptyFilter())
  const [settings, setSettings] = useState<SettingsView>({
    display_currency: 'CNY',
    usd_cny_rate: 7.1,
    collector_upstream: 'https://api.deepseek.com',
  })
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [editingId, setEditingId] = useState<number | null>(null)
  const [draft, setDraft] = useState<EditDraft>({})

  const cc = useMemo(
    () => ({ display: (settings.display_currency as 'CNY' | 'USD') || 'CNY', usdCnyRate: settings.usd_cny_rate }),
    [settings],
  )

  const load = useCallback((f: RecordFilter, p: number) => {
    setLoading(true)
    setError(null)
    listRecords(f, p, PAGE_SIZE)
      .then((r) => {
        setRows(r.rows)
        setTotal(r.total)
        setPage(p)
        setLoading(false)
      })
      .catch((e) => {
        setError(String(e))
        setLoading(false)
      })
  }, [])

  useEffect(() => {
    getSettings().then(setSettings).catch(() => {})
    load(filter, 0)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))

  function applyFilter() {
    load(filter, 0)
  }

  function resetFilter() {
    const f = emptyFilter()
    setFilter(f)
    load(f, 0)
  }

  async function handleDelete(id: number) {
    if (!window.confirm('确认删除这条记录？此操作不可撤销。')) return
    try {
      await deleteRecord(id)
      load(filter, page)
    } catch (e) {
      setError(String(e))
    }
  }

  function startEdit(r: UsageRecord) {
    setEditingId(r.id)
    setDraft({
      recorded_at: r.recorded_at,
      provider_code: r.provider_code,
      model_name: r.model_name,
      session_id: r.session_id,
      prompt_tokens: r.prompt_tokens,
      completion_tokens: r.completion_tokens,
      cached_tokens: r.cached_tokens,
      cost_usd: r.cost_usd,
      project: r.project,
      tags: tagsList(r.tags),
      note: r.note,
    })
  }

  async function saveEdit() {
    if (editingId === null) return
    try {
      await updateRecord(editingId, {
        recorded_at: draft.recorded_at || undefined,
        provider_code: draft.provider_code ?? null,
        model_name: draft.model_name ?? null,
        session_id: draft.session_id ?? null,
        project: draft.project ?? null,
        note: draft.note ?? null,
        tags: draft.tags,
        prompt_tokens: draft.prompt_tokens ?? null,
        completion_tokens: draft.completion_tokens ?? null,
        cached_tokens: draft.cached_tokens ?? null,
        recompute_cost: true,
      })
      setEditingId(null)
      load(filter, page)
    } catch (e) {
      setError(String(e))
    }
  }

  function parseTags(v: string): string[] {
    return v
      .split(/[,，;；]/)
      .map((s) => s.trim())
      .filter(Boolean)
  }

  return (
    <div className="page">
      <div className="page-header">
        <h2>用量明细</h2>
        <span className="muted">
          共 {total} 条 · 第 {page + 1}/{totalPages} 页
        </span>
      </div>

      {error && <div className="error-box">{error}</div>}

      <div className="filter-bar">
        <input
          type="date"
          value={filter.from ?? ''}
          onChange={(e) => setFilter({ ...filter, from: e.target.value })}
          title="开始日期"
        />
        <input
          type="date"
          value={filter.to ?? ''}
          onChange={(e) => setFilter({ ...filter, to: e.target.value })}
          title="结束日期"
        />
        <input
          placeholder="厂商(如 deepseek)"
          value={filter.provider_code ?? ''}
          onChange={(e) => setFilter({ ...filter, provider_code: e.target.value })}
        />
        <input
          placeholder="模型"
          value={filter.model_name ?? ''}
          onChange={(e) => setFilter({ ...filter, model_name: e.target.value })}
        />
        <input
          placeholder="会话/请求关键词"
          value={filter.session_keyword ?? ''}
          onChange={(e) => setFilter({ ...filter, session_keyword: e.target.value })}
        />
        <button className="btn primary" onClick={applyFilter}>
          筛选
        </button>
        <button className="btn" onClick={resetFilter}>
          重置
        </button>
      </div>

      {loading && <p className="loading">加载中…</p>}

      {!loading && (
        <div className="table-wrap">
          <table className="data-table">
            <thead>
              <tr>
                <th>时间</th>
                <th>厂商/模型</th>
                <th>会话</th>
                <th>输入</th>
                <th>输出</th>
                <th>缓存</th>
                <th>总Token</th>
                <th>费用</th>
                <th>来源</th>
                <th>项目/标签</th>
                <th>操作</th>
              </tr>
            </thead>
            <tbody>
              {rows.length === 0 && (
                <tr>
                  <td colSpan={11} className="muted center">
                    暂无数据
                  </td>
                </tr>
              )}
              {rows.map((r) =>
                editingId === r.id ? (
                  <tr key={r.id} className="editing">
                    <td>
                      <input
                        type="datetime-local"
                        value={toLocalInput(r.recorded_at)}
                        onChange={(e) => setDraft({ ...draft, recorded_at: toIso(e.target.value) })}
                      />
                    </td>
                    <td>
                      <input
                        placeholder="模型"
                        defaultValue={r.model_name ?? ''}
                        onChange={(e) => setDraft({ ...draft, model_name: e.target.value || null })}
                      />
                    </td>
                    <td>
                      <input
                        placeholder="会话ID"
                        defaultValue={r.session_id ?? ''}
                        onChange={(e) => setDraft({ ...draft, session_id: e.target.value || null })}
                      />
                    </td>
                    <td>
                      <input
                        type="number"
                        defaultValue={r.prompt_tokens ?? ''}
                        onChange={(e) =>
                          setDraft({ ...draft, prompt_tokens: e.target.value ? Number(e.target.value) : null })
                        }
                      />
                    </td>
                    <td>
                      <input
                        type="number"
                        defaultValue={r.completion_tokens ?? ''}
                        onChange={(e) =>
                          setDraft({
                            ...draft,
                            completion_tokens: e.target.value ? Number(e.target.value) : null,
                          })
                        }
                      />
                    </td>
                    <td>
                      <input
                        type="number"
                        defaultValue={r.cached_tokens ?? ''}
                        onChange={(e) =>
                          setDraft({ ...draft, cached_tokens: e.target.value ? Number(e.target.value) : null })
                        }
                      />
                    </td>
                    <td className="num">{fmtTokens(r.total_tokens)}</td>
                    <td className="num">
                      <input
                        type="number"
                        step="0.0001"
                        defaultValue={r.cost_usd ?? ''}
                        onChange={(e) =>
                          setDraft({ ...draft, cost_usd: e.target.value ? Number(e.target.value) : null })
                        }
                      />
                    </td>
                    <td>{r.source}</td>
                    <td>
                      <input
                        placeholder="项目/标签,分隔"
                        defaultValue={[r.project ?? '', tagsText(r.tags)].filter(Boolean).join(' / ')}
                        onChange={(e) => {
                          const text = e.target.value
                          setDraft({
                            ...draft,
                            project: text.split('/')[0]?.trim() || null,
                            tags: parseTags(text.split('/')[1] ?? ''),
                          })
                        }}
                      />
                    </td>
                    <td className="ops">
                      <button className="btn small" onClick={saveEdit}>
                        保存
                      </button>
                      <button className="btn small" onClick={() => setEditingId(null)}>
                        取消
                      </button>
                    </td>
                  </tr>
                ) : (
                  <tr key={r.id}>
                    <td>{fmtDate(r.recorded_at)}</td>
                    <td>
                      <span className="provider">{r.provider_code ?? '-'}</span>{' '}
                      {r.model_name ?? '-'}
                    </td>
                    <td title={r.session_id ?? ''} className="ellipsis">
                      {r.session_id ?? (r.request_id ? `req:${r.request_id}` : '-')}
                    </td>
                    <td className="num">{fmtTokens(r.prompt_tokens)}</td>
                    <td className="num">{fmtTokens(r.completion_tokens)}</td>
                    <td className="num">{fmtTokens(r.cached_tokens)}</td>
                    <td className="num">{fmtTokens(r.total_tokens)}</td>
                    <td className="num">{displayCost(r.cost_usd, cc)}</td>
                    <td>
                      {r.source}
                      {r.cost_source === 'computed' && <span className="tag">估算</span>}
                    </td>
                    <td>
                      {r.project && <span className="tag tag-project">{r.project}</span>}
                      {tagsList(r.tags).map((t) => (
                        <span key={t} className="tag">
                          {t}
                        </span>
                      ))}
                    </td>
                    <td className="ops">
                      <button className="btn small" onClick={() => startEdit(r)}>
                        编辑
                      </button>
                      <button className="btn small danger" onClick={() => handleDelete(r.id)}>
                        删除
                      </button>
                    </td>
                  </tr>
                ),
              )}
            </tbody>
          </table>
        </div>
      )}

      <div className="pager">
        <button className="btn small" disabled={page === 0} onClick={() => load(filter, page - 1)}>
          ← 上一页
        </button>
        <button
          className="btn small"
          disabled={page + 1 >= totalPages}
          onClick={() => load(filter, page + 1)}
        >
          下一页 →
        </button>
      </div>
    </div>
  )
}

function tagsText(tags: string | null): string {
  return tagsList(tags).join(',')
}

function tagsList(tags: string | null): string[] {
  if (!tags) return []
  try {
    const arr = JSON.parse(tags)
    return Array.isArray(arr) ? arr.filter((x) => typeof x === 'string') : []
  } catch {
    return []
  }
}

function toLocalInput(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso.slice(0, 16)
  const pad = (x: number) => String(x).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`
}

function toIso(local: string): string {
  if (!local) return ''
  return new Date(local).toISOString()
}
