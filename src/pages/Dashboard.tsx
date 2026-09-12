import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { EChartsOption } from 'echarts'

import {
  claudeCodeStatus,
  codexStatus,
  collectorStatus,
  getDashboard,
  getHourlyTrend,
  getSettings,
  listRecords,
  workbuddyStatus,
  type ClaudeCodeWatcherInfo,
  type CodexWatcherInfo,
  type CollectorStatusInfo,
  type DashboardData,
  type DistBucket,
  type HourTrendPoint,
  type RecordFilter,
  type UsageRecord,
  type WorkBuddyWatcherInfo,
} from '../api/client'
import type { SettingsView } from '../api/client'
import Chart from '../components/Chart'
import {
  costNumber,
  daysAgoLocal,
  displayCost,
  fmtClock,
  fmtTokens,
  todayLocal,
} from '../utils/format'

/** 轮询间隔(ms) */
const POLL_MS = 5000

const RANGES = [
  { key: 'today', label: '今日' },
  { key: '7', label: '近7天' },
  { key: '30', label: '近30天' },
  { key: '', label: '全部' },
]

/** 采集来源(软件) → 展示名 */
const SOURCE_LABELS: Record<string, string> = {
  dsh: 'DSH',
  claude_code: 'Claude Code',
  codex: 'Codex',
  workbuddy: 'WorkBuddy',
  proxy: '本地代理',
  collector: 'HTTP 上报',
  manual: '手动录入',
}
const sourceLabel = (s: string): string => SOURCE_LABELS[s] ?? s

interface ViewData {
  dash: DashboardData
  hourly: HourTrendPoint[]
  recent: UsageRecord[]
  status: CollectorStatusInfo | null
  ccStatus: ClaudeCodeWatcherInfo | null
  cxStatus: CodexWatcherInfo | null
  wbStatus: WorkBuddyWatcherInfo | null
  /** 各来源(软件)用量; 后端忽略 source 自身筛选, 便于标签栏始终显示各家对比 */
  sources: DistBucket[]
}

export default function Dashboard() {
  const [vd, setVd] = useState<ViewData | null>(null)
  const [settings, setSettings] = useState<SettingsView>({
    display_currency: 'CNY',
    usd_cny_rate: 7.1,
    collector_upstream: 'https://api.deepseek.com',
  })
  const [range, setRange] = useState('today')
  /** 采集来源筛选: '' = 全部软件 */
  const [source, setSource] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null)
  const rangeRef = useRef(range)
  rangeRef.current = range
  const sourceRef = useRef(source)
  sourceRef.current = source

  useEffect(() => {
    getSettings().then(setSettings).catch(() => {})
  }, [])

  const buildFilter = useCallback((r: string, src: string): RecordFilter => {
    const f: RecordFilter = {}
    if (r === 'today') {
      f.from = todayLocal()
      f.to = todayLocal()
    } else if (r === '7' || r === '30') {
      f.from = daysAgoLocal(Number(r) - 1)
      f.to = todayLocal()
    }
    if (src) f.source = src
    return f
  }, [])

  const refresh = useCallback(async () => {
    const f = buildFilter(rangeRef.current, sourceRef.current)
    // "最近检测到" 是活动流: 只按来源过滤, 不受日期范围限制(保留原行为)
    const recentFilter: RecordFilter = sourceRef.current ? { source: sourceRef.current } : {}
    try {
      const [dash, hourly, rc, st, ccSt, cxSt, wbSt] = await Promise.all([
        getDashboard(f),
        getHourlyTrend(f),
        listRecords(recentFilter, 0, 15),
        collectorStatus(),
        claudeCodeStatus(),
        codexStatus(),
        workbuddyStatus(),
      ])
      setVd({
        dash,
        hourly,
        recent: rc.rows,
        status: st,
        ccStatus: ccSt,
        cxStatus: cxSt,
        wbStatus: wbSt,
        sources: dash.by_source ?? [],
      })
      setError(null)
      setLastUpdated(new Date())
    } catch (e) {
      setError(String(e))
    }
  }, [buildFilter])

  useEffect(() => {
    refresh()
  }, [refresh, range, source])

  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') refresh()
    }, POLL_MS)
    return () => window.clearInterval(timer)
  }, [refresh])

  const cc = useMemo(
    () => ({ display: (settings.display_currency as 'CNY' | 'USD') || 'CNY', usdCnyRate: settings.usd_cny_rate }),
    [settings],
  )

  const rangeLabel = RANGES.find((r) => r.key === range)?.label ?? '全部'
  const o = vd?.dash.overview
  const monitoring = vd?.status?.started
  const isToday = range === 'today'

  // 主图: 今日→24h柱状(自动检测视角), 其它→按日趋势
  const trendOption = useMemo<EChartsOption>(() => {
    if (isToday) {
      const points = vd?.hourly ?? []
      const labels = points.map((p) => `${Number(p.hour.slice(11))}:00`)
      return {
        tooltip: { trigger: 'axis', valueFormatter: (v) => fmtTokens(Number(v)) },
        grid: { left: 64, right: 24, top: 24, bottom: 24 },
        xAxis: {
          type: 'category',
          data: labels,
          axisLabel: { interval: 3 }, // 每 4 小时标一个
        },
        yAxis: [{ type: 'value', name: 'Tokens' }],
        series: [
          {
            name: 'Token',
            type: 'bar',
            data: points.map((p) => p.total_tokens),
            itemStyle: { color: '#2563eb', borderRadius: [3, 3, 0, 0] },
            barMaxWidth: 14,
          },
        ],
      }
    }
    const points = vd?.dash.trend ?? []
    return {
      tooltip: { trigger: 'axis', valueFormatter: (v) => fmtTokens(Number(v)) },
      legend: { data: ['总Token', '费用'] },
      grid: { left: 64, right: 64, top: 32, bottom: 24 },
      xAxis: { type: 'category', data: points.map((p) => p.day.slice(5)) },
      yAxis: [
        { type: 'value', name: 'Tokens' },
        { type: 'value', name: '费用', splitLine: { show: false } },
      ],
      series: [
        {
          name: '总Token',
          type: 'bar',
          data: points.map((p) => p.total_tokens),
          itemStyle: { color: '#2563eb', borderRadius: [3, 3, 0, 0] },
        },
        {
          name: '费用',
          type: 'line',
          yAxisIndex: 1,
          smooth: true,
          data: points.map((p) => costNumber(p.cost_usd, cc)),
          itemStyle: { color: '#f59e0b' },
        },
      ],
    }
  }, [vd, isToday, cc])

  const maxModelTokens = Math.max(1, ...(vd?.dash.by_model ?? []).map((m) => m.total_tokens))
  /** 全部软件(供标签栏切换用; 后端已忽略 source 自身筛选, 故始终完整) */
  const allSources = vd?.sources ?? []
  /** 面板展示行: 选中某软件时只显示该软件, 与其它面板保持同一筛选口径 */
  const panelSources = source ? allSources.filter((s) => s.key === source) : allSources
  const maxSourceTokens = Math.max(1, ...panelSources.map((s) => s.total_tokens))
  /**
   * 标签栏键: 历史全部来源 ∪ 当前时间段有数据的来源。
   * 必须用全量列表 —— 否则跨天/换时间段后, 没数据的软件连标签一起消失,
   * 用户会以为数据丢了(实际只是不在当前时间段)。
   */
  const chipKeys = (() => {
    const inRange = allSources.map((s) => s.key)
    const known = vd?.dash.sources_all ?? []
    const extra = known.filter((k) => !inRange.includes(k)).sort()
    return [...inRange, ...extra]
  })()

  /** 正在运行的自动检测通道(可叠加) */
  const activeChannels = [
    monitoring ? 'DSH' : null,
    vd?.ccStatus?.started ? 'Claude Code' : null,
    vd?.cxStatus?.started ? 'Codex' : null,
    vd?.wbStatus?.started ? 'WorkBuddy' : null,
  ].filter((x): x is string => x !== null)

  return (
    <div className="page dash-page">
      {/* ── 检测状态横幅 ── */}
      <div className={`monitor-bar ${monitoring ? 'on' : 'off'}`}>
        <span className="dot" />
        <strong>
          {activeChannels.length
            ? `${activeChannels.join(' + ')} 自动检测中`
            : '检测服务未运行'}
        </strong>
        <span className="muted">
          {lastUpdated && `每 ${POLL_MS / 1000}s 自动刷新 · 更新于 ${fmtClock(lastUpdated)}`}
        </span>
        {!monitoring && vd?.status?.error && <span className="warn-text">（{vd.status.error}）</span>}
        {vd && <span className="spacer" />}
        <div className="seg">
          {RANGES.map((r) => (
            <button
              key={r.key}
              className={`seg-btn ${range === r.key ? 'active' : ''}`}
              onClick={() => setRange(r.key)}
            >
              {r.label}
            </button>
          ))}
        </div>
      </div>

      {/* ── 采集来源(软件)筛选标签 ── */}
      <div className="src-bar">
        <span className="src-label">采集来源</span>
        <button
          className={`src-chip ${source === '' ? 'active' : ''}`}
          onClick={() => setSource('')}
        >
          全部
        </button>
        {chipKeys.map((k) => {
          const bucket = allSources.find((s) => s.key === k)
          const tokens = bucket?.total_tokens ?? 0
          const isEmpty = tokens === 0
          return (
            <button
              key={k}
              className={`src-chip ${source === k ? 'active' : ''} ${isEmpty ? 'empty' : ''}`}
              onClick={() => setSource(k)}
              title={
                isEmpty
                  ? `${sourceLabel(k)}：${rangeLabel}无数据（历史有记录，可切换到「全部」查看）`
                  : `${bucket?.record_count ?? 0} 条记录 · ${rangeLabel}`
              }
            >
              {sourceLabel(k)}
              <em>{fmtTokens(tokens)}</em>
            </button>
          )
        })}
        {vd && chipKeys.length === 0 && (
          <span className="muted">暂无来源数据</span>
        )}
      </div>

      {error && (
        <div className="error-box">
          加载失败: {error}
          {vd && <span className="muted">（下方为上次成功加载的数据）</span>}
        </div>
      )}

      {vd && (
        <>
          {/* ── 核心大数字区 ── */}
          <div className="hero-grid">
            <div className="hero-card hero-main">
              <div className="hero-label">{rangeLabel}总Token</div>
              <div className="hero-value">{fmtTokens(o?.total_tokens)}</div>
              <div className="hero-sub">
                输入 {fmtTokens(o?.prompt_tokens)} · 输出 {fmtTokens(o?.completion_tokens)}
                {o && o.cached_tokens > 0 && ` · 缓存 ${fmtTokens(o.cached_tokens)}`}
              </div>
            </div>
            <div className="hero-card">
              <div className="hero-label">{rangeLabel}估算费用</div>
              <div className="hero-value accent-value">{displayCost(o?.cost_usd, cc)}</div>
              <div className="hero-sub">
                {o && o.cost_usd === null ? '单价缺失→仅按官方费用计' : `成本口径: ${cc.display}`}
              </div>
            </div>
            <div className="hero-card">
              <div className="hero-label">上报次数</div>
              <div className="hero-value">{o?.record_count ?? 0}</div>
              <div className="hero-sub">覆盖 {o?.day_count ?? 0} 天</div>
            </div>
            <div className="hero-card">
              <div className="hero-label">使用模型</div>
              <div className="hero-value">{o?.model_count ?? 0}</div>
              <div className="hero-sub">{o?.provider_count ?? 0} 家厂商</div>
            </div>
          </div>

          <div className="dash-mid">
            {/* ── 趋势/24h ── */}
            <div className="panel">
              <h3>{isToday ? '今日 24 小时用量' : `${rangeLabel}用量趋势`}</h3>
              {isToday && vd.hourly.length === 0 && (
                <p className="muted">今日暂无上报——把程序里的调用 POST 到收集地址即可自动点亮此图</p>
              )}
              <Chart option={trendOption} height={250} />
            </div>

            {/* ── 模型排行(自绘占比条) ── */}
            <div className="panel">
              <h3>模型用量排行</h3>
              {(vd.dash.by_model ?? []).length === 0 ? (
                <p className="muted">暂无数据</p>
              ) : (
                <div className="rank-list">
                  {(vd.dash.by_model ?? []).map((m) => (
                    <div className="rank-row" key={m.key}>
                      <div className="rank-head">
                        <span className="rank-name">{m.key}</span>
                        <span className="rank-num">
                          {fmtTokens(m.total_tokens)}
                          <em>{displayCost(m.cost_usd, cc)}</em>
                        </span>
                      </div>
                      <div className="rank-bar-bg">
                        <div
                          className="rank-bar"
                          style={{ width: `${(m.total_tokens / maxModelTokens) * 100}%` }}
                        />
                      </div>
                      <div className="rank-foot">{m.record_count} 次上报</div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>

          {/* ── 按软件(采集来源)拆分 ── */}
          <div className="panel">
            <h3>
              {source ? `${sourceLabel(source)} 用量` : '按软件拆分'}（{rangeLabel}）
            </h3>
            {panelSources.length === 0 ? (
              <p className="muted">
                {source
                  ? `${sourceLabel(source)} 在「${rangeLabel}」范围内没有记录 —— 数据可能仍在，试试切换到「全部」时间段。`
                  : '暂无数据'}
              </p>
            ) : (
              <div className="rank-list">
                {panelSources.map((s) => (
                  <div
                    className="rank-row"
                    key={s.key}
                    onClick={() => setSource(source === s.key ? '' : s.key)}
                    style={{ cursor: 'pointer' }}
                    title="点击筛选该来源"
                  >
                    <div className="rank-head">
                      <span className="rank-name">
                        {sourceLabel(s.key)}
                        {source === s.key && <span className="tag">筛选中</span>}
                      </span>
                      <span className="rank-num">
                        {fmtTokens(s.total_tokens)}
                        <em>{displayCost(s.cost_usd, cc)}</em>
                      </span>
                    </div>
                    <div className="rank-bar-bg">
                      <div
                        className="rank-bar"
                        style={{ width: `${(s.total_tokens / maxSourceTokens) * 100}%` }}
                      />
                    </div>
                    <div className="rank-foot">{s.record_count} 次记录</div>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* ── 最近检测实时流 ── */}
          <div className="panel">
            <h3>最近检测到（自动刷新）</h3>
            {vd.recent.length === 0 ? (
              <p className="muted">还没有记录。到「上报」页测试，或在本机程序调用后把用量 POST 到收集地址。</p>
            ) : (
              <table className="data-table recent-table">
                <tbody>
                  {vd.recent.map((r, idx) => (
                    <tr key={r.id} className={idx === 0 ? 'fresh' : ''}>
                      <td className="num time">{fmtDateCol(r.recorded_at)}</td>
                      <td className="recent-model">
                        {r.provider_code && (
                          <>
                            <span className="provider">{r.provider_code}</span>{' '}
                          </>
                        )}
                        {r.model_name}
                      </td>
                      <td className="num recent-tokens">
                        ↑{fmtTokens(r.prompt_tokens)} ↓{fmtTokens(r.completion_tokens)}
                      </td>
                      <td className="num">{displayCost(r.cost_usd, cc)}</td>
                      <td>
                        <span className={`tag ${r.source === 'dsh' ? 'tag-project' : ''}`}>
                          {r.source === 'dsh'
                            ? 'DSH'
                            : r.source === 'claude_code'
                              ? 'Claude'
                              : r.source === 'collector'
                                ? '检测'
                                : r.source}
                        </span>
                        {r.cost_source === 'computed' && <span className="tag">估算</span>}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>

          {/* ── 自动检测通道状态 ── */}
          <div className="panel">
            <h3>自动检测通道</h3>
            <div className="muted" style={{ marginBottom: 8 }}>
              零配置：应用启动后每 3 秒增量读取本地用量文件并自动入库，重启不会重复计数。
            </div>
            <p className={vd.ccStatus?.started ? 'ok-text' : 'warn-text'}>
              {vd.ccStatus?.started ? '✓' : '✗'} <strong>Claude Code</strong>{' '}
              {vd.ccStatus?.started ? (
                <>
                  运行中 · <code>{vd.ccStatus.claude_home}\projects\**\*.jsonl</code>
                </>
              ) : (
                <>未启动{vd.ccStatus?.error ? ` · ${vd.ccStatus.error}` : ''}</>
              )}
            </p>
            <p className={vd.cxStatus?.started ? 'ok-text' : 'warn-text'}>
              {vd.cxStatus?.started ? '✓' : '✗'} <strong>Codex</strong>{' '}
              {vd.cxStatus?.started ? (
                <>
                  运行中 · <code>{vd.cxStatus.codex_home}\sessions\**\rollout-*.jsonl</code>
                </>
              ) : (
                <>未启动{vd.cxStatus?.error ? ` · ${vd.cxStatus.error}` : ''}</>
              )}
            </p>
            <p className={vd.wbStatus?.started ? 'ok-text' : 'warn-text'}>
              {vd.wbStatus?.started ? '✓' : '✗'} <strong>WorkBuddy</strong>{' '}
              {vd.wbStatus?.started ? (
                <>
                  运行中 · <code>{vd.wbStatus.workbuddy_home}\projects\**\*.jsonl</code>
                </>
              ) : (
                <>未启动{vd.wbStatus?.error ? ` · ${vd.wbStatus.error}` : ''}</>
              )}
            </p>
          </div>
        </>
      )}
      {!vd && !error && <p className="loading">加载中…</p>}
    </div>
  )
}

function fmtDateCol(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  const pad = (x: number) => String(x).padStart(2, '0')
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
}
