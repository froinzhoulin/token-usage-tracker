import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { EChartsOption } from 'echarts'

import {
  collectorStatus,
  getDashboard,
  getHourlyTrend,
  getSettings,
  listRecords,
  type CollectorStatusInfo,
  type DashboardData,
  type HourTrendPoint,
  type RecordFilter,
  type UsageRecord,
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

interface ViewData {
  dash: DashboardData
  hourly: HourTrendPoint[]
  recent: UsageRecord[]
  status: CollectorStatusInfo | null
}

export default function Dashboard() {
  const [vd, setVd] = useState<ViewData | null>(null)
  const [settings, setSettings] = useState<SettingsView>({
    display_currency: 'CNY',
    usd_cny_rate: 7.1,
    collector_upstream: 'https://api.deepseek.com',
  })
  const [range, setRange] = useState('today')
  const [error, setError] = useState<string | null>(null)
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null)
  const rangeRef = useRef(range)
  rangeRef.current = range

  useEffect(() => {
    getSettings().then(setSettings).catch(() => {})
  }, [])

  const buildFilter = useCallback((r: string): RecordFilter => {
    if (r === 'today') return { from: todayLocal(), to: todayLocal() }
    if (r === '7' || r === '30') return { from: daysAgoLocal(Number(r) - 1), to: todayLocal() }
    return {}
  }, [])

  const refresh = useCallback(async () => {
    const f = buildFilter(rangeRef.current)
    try {
      const [dash, hourly, rc, st] = await Promise.all([
        getDashboard(f),
        getHourlyTrend(f),
        listRecords({}, 0, 15),
        collectorStatus(),
      ])
      setVd({ dash, hourly, recent: rc.rows, status: st })
      setError(null)
      setLastUpdated(new Date())
    } catch (e) {
      setError(String(e))
    }
  }, [buildFilter])

  useEffect(() => {
    refresh()
  }, [refresh, range])

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

  return (
    <div className="page dash-page">
      {/* ── 检测状态横幅 ── */}
      <div className={`monitor-bar ${monitoring ? 'on' : 'off'}`}>
        <span className="dot" />
        <strong>{monitoring ? '检测中' : '未运行'}</strong>
        <span className="muted">
          {vd?.status ? `上报地址 ${vd.status.base_url}/api/v1/usage` : ''}
          {lastUpdated && ` · 每 ${POLL_MS / 1000}s 自动刷新 · 更新于 ${fmtClock(lastUpdated)}`}
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

      {error && !vd && <div className="error-box">加载失败: {error}</div>}

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
                        <span className="provider">{r.provider_code ?? '?'}</span> {r.model_name}
                      </td>
                      <td className="num recent-tokens">
                        ↑{fmtTokens(r.prompt_tokens)} ↓{fmtTokens(r.completion_tokens)}
                      </td>
                      <td className="num">{displayCost(r.cost_usd, cc)}</td>
                      <td>
                        <span className={`tag ${r.source === 'collector' ? 'tag-project' : ''}`}>
                          {r.source === 'collector' ? '检测' : r.source}
                        </span>
                        {r.cost_source === 'computed' && <span className="tag">估算</span>}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
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
