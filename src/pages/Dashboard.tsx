import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { EChartsOption } from 'echarts'

import {
  collectorStatus,
  getDashboard,
  getSettings,
  listRecords,
  type CollectorStatusInfo,
  type DashboardData,
  type RecordFilter,
  type UsageRecord,
} from '../api/client'
import type { SettingsView } from '../api/client'
import Chart from '../components/Chart'
import StatCard from '../components/StatCard'
import {
  costNumber,
  currencySymbol,
  daysAgoLocal,
  displayCost,
  fmtClock,
  fmtCostNumber,
  fmtDate,
  fmtTokens,
  todayLocal,
} from '../utils/format'

/** 轮询间隔(ms): 自动检测新上报 */
const POLL_MS = 5000

const RANGES = [
  { key: 'today', label: '今日' },
  { key: '7', label: '近7天' },
  { key: '30', label: '近30天' },
  { key: '', label: '全部' },
]

export default function Dashboard() {
  const [data, setData] = useState<DashboardData | null>(null)
  const [recent, setRecent] = useState<UsageRecord[]>([])
  const [status, setStatus] = useState<CollectorStatusInfo | null>(null)
  const [settings, setSettings] = useState<SettingsView>({
    display_currency: 'CNY',
    usd_cny_rate: 7.1,
  })
  const [range, setRange] = useState('today')
  const [error, setError] = useState<string | null>(null)
  const [firstLoad, setFirstLoad] = useState(true)
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null)
  const rangeRef = useRef(range)
  rangeRef.current = range

  useEffect(() => {
    getSettings().then(setSettings).catch(() => {})
  }, [])

  const buildFilter = useCallback((r: string): RecordFilter => {
    if (r === 'today') {
      return { from: todayLocal(), to: todayLocal() }
    }
    if (r === '7' || r === '30') {
      return { from: daysAgoLocal(Number(r) - 1), to: todayLocal() }
    }
    return {}
  }, [])

  const refresh = useCallback(async () => {
    const f = buildFilter(rangeRef.current)
    try {
      const [d, rc, st] = await Promise.all([
        getDashboard(f),
        listRecords({}, 0, 12),
        collectorStatus(),
      ])
      setData(d)
      setRecent(rc.rows)
      setStatus(st)
      setError(null)
      setLastUpdated(new Date())
    } catch (e) {
      setError(String(e))
    } finally {
      setFirstLoad(false)
    }
  }, [buildFilter])

  // 首次 + 范围切换刷新
  useEffect(() => {
    refresh()
  }, [refresh, range])

  // 自动轮询: 有新上报自动上屏
  useEffect(() => {
    const timer = window.setInterval(() => {
      // 页面可见时才轮询, 避免后台空转
      if (document.visibilityState === 'visible') refresh()
    }, POLL_MS)
    return () => window.clearInterval(timer)
  }, [refresh])

  const cc = useMemo(
    () => ({
      display: (settings.display_currency as 'CNY' | 'USD') || 'CNY',
      usdCnyRate: settings.usd_cny_rate,
    }),
    [settings],
  )

  const rangeLabel = RANGES.find((r) => r.key === range)?.label ?? '全部'

  const trendOption = useMemo<EChartsOption>(() => {
    const points = data?.trend ?? []
    return {
      tooltip: {
        trigger: 'axis',
        valueFormatter: (v) => fmtTokens(Number(v)),
      },
      legend: { data: ['总Token', '输入', '输出', '费用'] },
      grid: { left: 60, right: 20, top: 40, bottom: 30 },
      xAxis: { type: 'category', data: points.map((p) => p.day) },
      yAxis: [
        { type: 'value', name: 'Tokens' },
        { type: 'value', name: '费用', splitLine: { show: false } },
      ],
      series: [
        {
          name: '总Token',
          type: 'line',
          smooth: true,
          data: points.map((p) => p.total_tokens),
          areaStyle: { opacity: 0.15 },
        },
        { name: '输入', type: 'line', smooth: true, data: points.map((p) => p.prompt_tokens) },
        { name: '输出', type: 'line', smooth: true, data: points.map((p) => p.completion_tokens) },
        {
          name: '费用',
          type: 'bar',
          yAxisIndex: 1,
          data: points.map((p) => costNumber(p.cost_usd, cc)),
          itemStyle: { color: '#f59e0b' },
        },
      ],
    }
  }, [data, cc])

  const modelDist = useMemo<EChartsOption>(() => {
    const items = data?.by_model ?? []
    return {
      tooltip: {
        trigger: 'item',
        formatter: (p: unknown) => {
          const it = p as { name: string; value: number }
          return `${it.name}<br/>${fmtTokens(it.value)} tokens`
        },
      },
      series: [
        {
          type: 'pie',
          radius: ['35%', '65%'],
          data: items.map((d) => ({ name: d.key, value: d.total_tokens })),
          label: { show: false },
        },
      ],
    }
  }, [data])

  const o = data?.overview
  const monitoring = status?.started

  return (
    <div className="page">
      <div className={`monitor-bar ${monitoring ? 'on' : 'off'}`}>
        <span className="dot" />
        <strong>{monitoring ? '检测中' : '未运行'}</strong>
        <span className="muted">
          {status ? `上报地址 ${status.base_url}/api/v1/usage` : '收集服务未启动'}
          {lastUpdated && ` · 每 ${POLL_MS / 1000}s 自动刷新 · 更新于 ${fmtClock(lastUpdated)}`}
        </span>
        {!monitoring && status?.error && <span className="warn-text">（{status.error}）</span>}
      </div>

      <div className="page-header">
        <h2>用量看板</h2>
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

      {error && !data && <div className="error-box">加载失败: {error}</div>}
      {firstLoad && !data && <p className="loading">加载中…</p>}

      {data && (
        <>
          <div className="stat-grid">
            <StatCard
              label={`${rangeLabel} Token`}
              value={fmtTokens(o?.total_tokens)}
              hint={`${o?.record_count ?? 0} 条上报`}
              accent
            />
            <StatCard
              label={`${rangeLabel} 费用`}
              value={displayCost(o?.cost_usd, cc)}
              hint={o && o.cost_usd === null ? '尚无费用数据(单价缺失则按估算)' : `覆盖 ${o?.day_count ?? 0} 天`}
              accent
            />
            <StatCard label="输入 Token" value={fmtTokens(o?.prompt_tokens)} hint={`缓存命中 ${fmtTokens(o?.cached_tokens)}`} />
            <StatCard label="输出 Token" value={fmtTokens(o?.completion_tokens)} />
            <StatCard label="模型数" value={o?.model_count ?? 0} hint={`${o?.provider_count ?? 0} 个厂商`} />
          </div>

          <div className="panel">
            <h3>趋势（{rangeLabel}）</h3>
            <Chart option={trendOption} height={280} />
          </div>

          <div className="panel-grid">
            <div className="panel">
              <h3>按模型分布（{rangeLabel}）</h3>
              <Chart option={modelDist} height={230} />
              <DistTable items={data.by_model ?? []} cc={cc} symbol={currencySymbol(cc)} />
            </div>

            <div className="panel">
              <h3>最近检测到（自动刷新）</h3>
              {recent.length === 0 ? (
                <p className="muted">
                  还没有记录。到「上报」页测试，或在本机程序调用后把用量 POST 到收集地址。
                </p>
              ) : (
                <table className="data-table recent-table">
                  <tbody>
                    {recent.map((r) => (
                      <tr key={r.id}>
                        <td className="num time">{fmtDate(r.recorded_at)}</td>
                        <td className="recent-model">
                          <span className="provider">{r.provider_code ?? '?'}</span> {r.model_name}
                        </td>
                        <td className="num">{fmtTokens(r.prompt_tokens)}→{fmtTokens(r.completion_tokens)}</td>
                        <td className="num">{displayCost(r.cost_usd, cc)}</td>
                        <td>
                          <span className={`tag ${r.source === 'collector' ? 'tag-project' : ''}`}>
                            {r.source === 'collector' ? '检测' : r.source}
                          </span>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>
          </div>
        </>
      )}
    </div>
  )
}

function DistTable({
  items,
  cc,
  symbol,
}: {
  items: { key: string; total_tokens: number; cost_usd: number | null; record_count: number }[]
  cc: { display: 'CNY' | 'USD'; usdCnyRate: number }
  symbol: string
}) {
  if (!items.length) return <p className="muted">暂无数据</p>
  return (
    <table className="mini-table">
      <tbody>
        {items.map((it) => (
          <tr key={it.key}>
            <td className="mini-key">{it.key}</td>
            <td>{fmtTokens(it.total_tokens)}</td>
            <td className="num">
              {symbol}
              {fmtCostNumber(costNumber(it.cost_usd, cc), cc).replace(/^[¥$]/, '')}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}
