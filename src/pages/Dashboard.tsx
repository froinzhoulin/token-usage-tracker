import { useEffect, useMemo, useState } from 'react'
import type { EChartsOption } from 'echarts'

import { getDashboard, getSettings, type DashboardData, type RecordFilter } from '../api/client'
import type { SettingsView } from '../api/client'
import Chart from '../components/Chart'
import StatCard from '../components/StatCard'
import {
  costNumber,
  currencySymbol,
  displayCost,
  fmtCostNumber,
  fmtTokens,
} from '../utils/format'

/** 快捷范围按钮 */
const RANGES = [
  { key: '', label: '全部' },
  { key: '30', label: '近30天' },
  { key: '90', label: '近90天' },
]

export default function Dashboard() {
  const [data, setData] = useState<DashboardData | null>(null)
  const [settings, setSettings] = useState<SettingsView>({
    display_currency: 'CNY',
    usd_cny_rate: 7.1,
  })
  const [range, setRange] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    getSettings().then(setSettings).catch(() => {})
  }, [])

  useEffect(() => {
    setLoading(true)
    setError(null)
    const filter: RecordFilter = {}
    if (range) {
      const to = new Date()
      const from = new Date()
      from.setDate(from.getDate() - Number(range))
      filter.from = from.toISOString().slice(0, 10)
      filter.to = to.toISOString().slice(0, 10)
    }
    getDashboard(filter)
      .then((d) => {
        setData(d)
        setLoading(false)
      })
      .catch((e) => {
        setError(String(e))
        setLoading(false)
      })
  }, [range])

  const cc = useMemo(
    () => ({ display: (settings.display_currency as 'CNY' | 'USD') || 'CNY', usdCnyRate: settings.usd_cny_rate }),
    [settings],
  )

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
        { name: '总Token', type: 'line', smooth: true, data: points.map((p) => p.total_tokens), areaStyle: { opacity: 0.15 } },
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

  const providerDist = useMemo<EChartsOption>(() => {
    const items = data?.by_provider ?? []
    return {
      tooltip: { trigger: 'axis' },
      grid: { left: 90, right: 20, top: 10, bottom: 30 },
      xAxis: { type: 'value' },
      yAxis: { type: 'category', data: items.map((d) => d.key).reverse() },
      series: [
        {
          type: 'bar',
          data: items.map((d) => d.total_tokens).reverse(),
          itemStyle: { color: '#2563eb' },
        },
      ],
    }
  }, [data])

  const o = data?.overview

  return (
    <div className="page">
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

      {error && <div className="error-box">加载失败: {error}</div>}
      {loading && <p className="loading">加载中…</p>}
      {!loading && data && (
        <>
          <div className="stat-grid">
            <StatCard label="总 Token" value={fmtTokens(o?.total_tokens)} accent />
            <StatCard label="总费用" value={displayCost(o?.cost_usd, cc)} hint={`约 ${o?.day_count ?? 0} 天 · ${o?.record_count ?? 0} 条记录`} accent />
            <StatCard label="输入 Token" value={fmtTokens(o?.prompt_tokens)} hint={`缓存命中 ${fmtTokens(o?.cached_tokens)}`} />
            <StatCard label="输出 Token" value={fmtTokens(o?.completion_tokens)} />
            <StatCard label="覆盖模型" value={o?.model_count ?? 0} hint={`${o?.provider_count ?? 0} 个厂商`} />
          </div>

          <div className="panel">
            <h3>用量趋势</h3>
            <Chart option={trendOption} height={320} />
          </div>

          <div className="panel-grid">
            <div className="panel">
              <h3>按模型分布</h3>
              <Chart option={modelDist} height={280} />
              <DistTable
                title="模型排行"
                items={data.by_model ?? []}
                cc={cc}
                currency={currencySymbol(cc)}
              />
            </div>
            <div className="panel">
              <h3>按厂商分布</h3>
              <Chart option={providerDist} height={220} />
              <DistTable
                title="厂商排行"
                items={data.by_provider ?? []}
                cc={cc}
                currency={currencySymbol(cc)}
              />
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
  currency,
}: {
  title?: string
  items: { key: string; total_tokens: number; cost_usd: number | null; record_count: number }[]
  cc: { display: 'CNY' | 'USD'; usdCnyRate: number }
  currency: string
}) {
  if (!items.length) return <p className="muted">暂无数据</p>
  return (
    <table className="mini-table">
      <tbody>
        {items.map((it) => (
          <tr key={it.key}>
            <td className="mini-key">{it.key}</td>
            <td>{fmtTokens(it.total_tokens)}</td>
            <td className="num">{currency}{fmtCostNumber(costNumber(it.cost_usd, cc), cc).replace(/^[¥$]/, '')}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}
