import { useEffect, useMemo, useState } from 'react'

import {
  backupDb,
  exportData,
  getSettings,
  listPrices,
  pickOpenPath,
  pickSavePath,
  restoreDb,
  setSettings,
  upsertCustomPrice,
  writeTextFile,
  type ModelPrice,
  type SettingsView,
} from '../api/client'
import { fmtCostNumber } from '../utils/format'

export default function SettingsPage() {
  const [settings, setSettingsState] = useState<SettingsView>({
    display_currency: 'CNY',
    usd_cny_rate: 7.1,
    collector_upstream: 'https://api.deepseek.com',
  })
  const [prices, setPrices] = useState<ModelPrice[]>([])
  const [error, setError] = useState<string | null>(null)
  const [okMsg, setOkMsg] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  // 自定义单价表单
  const [newPrice, setNewPrice] = useState({
    provider_code: '',
    provider_name: '',
    model_name: '',
    input: '',
    output: '',
    cached: '',
  })

  const cc = useMemo(
    () => ({
      display: (settings.display_currency as 'CNY' | 'USD') || 'CNY',
      usdCnyRate: settings.usd_cny_rate,
    }),
    [settings],
  )

  const flash = (msg: string, isErr = false) => {
    if (isErr) {
      setError(msg)
      setOkMsg(null)
    } else {
      setOkMsg(msg)
      setError(null)
    }
    window.setTimeout(() => {
      setError(null)
      setOkMsg(null)
    }, 5000)
  }

  useEffect(() => {
    getSettings().then(setSettingsState).catch((e) => flash(String(e), true))
    listPrices().then(setPrices).catch((e) => flash(String(e), true))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  async function saveSettings() {
    try {
      await setSettings(settings)
      flash('设置已保存')
    } catch (e) {
      flash(String(e), true)
    }
  }

  async function addCustomPrice() {
    const input = Number(newPrice.input)
    const output = Number(newPrice.output)
    if (!newPrice.model_name.trim()) {
      flash('请填写模型名', true)
      return
    }
    if (Number.isNaN(input) || Number.isNaN(output)) {
      flash('请输入合法的输入/输出单价', true)
      return
    }
    try {
      const providerCode = newPrice.provider_code.trim() || 'custom'
      await upsertCustomPrice({
        provider_code: providerCode,
        provider_name: newPrice.provider_name.trim() || providerCode,
        model_name: newPrice.model_name.trim(),
        input_per_mtok: input,
        output_per_mtok: output,
        cached_input_per_mtok: newPrice.cached.trim() ? Number(newPrice.cached) : null,
      })
      const ps = await listPrices()
      setPrices(ps)
      setNewPrice({ provider_code: '', provider_name: '', model_name: '', input: '', output: '', cached: '' })
      flash('自定义单价已保存')
    } catch (e) {
      flash(String(e), true)
    }
  }

  async function handleExport() {
    try {
      setBusy(true)
      const payload = await exportData({}, 'csv')
      const target = await pickSavePath(payload.file_name, ['csv'])
      if (!target) return
      await writeTextFile(target, payload.content)
      flash(`已导出 ${payload.rows} 条到 ${target}`)
    } catch (e) {
      flash(String(e), true)
    } finally {
      setBusy(false)
    }
  }

  async function handleBackup() {
    try {
      const target = await pickSavePath(`tracker-backup-${new Date().toISOString().slice(0, 10)}.db`, ['db'])
      if (!target) return
      await backupDb(target)
      flash(`备份完成: ${target}`)
    } catch (e) {
      flash(String(e), true)
    }
  }

  async function handleRestore() {
    if (!window.confirm('恢复将用备份文件覆盖当前全部数据，确认继续？')) return
    try {
      const src = await pickOpenPath(['db'], '备份文件')
      if (!src) return
      const n = await restoreDb(src)
      flash(`恢复成功，当前共 ${n} 条记录（建议重启应用刷新）`)
      window.setTimeout(() => window.location.reload(), 800)
    } catch (e) {
      flash(String(e), true)
    }
  }

  return (
    <div className="page">
      <div className="page-header">
        <h2>设置</h2>
      </div>

      {error && <div className="error-box">{error}</div>}
      {okMsg && <div className="ok-box">{okMsg}</div>}

      <div className="panel">
        <h3>透明代理上游（自动检测转发目标）</h3>
        <div className="row">
          <input
            className="wide-input"
            value={settings.collector_upstream}
            onChange={(e) => setSettingsState({ ...settings, collector_upstream: e.target.value })}
            placeholder="https://api.deepseek.com"
          />
          <button className="btn primary" onClick={saveSettings}>
            保存设置
          </button>
        </div>
        <p className="muted" style={{ marginTop: 6 }}>
          代理会把「上报」页所示地址收到的 /chat/completions 请求转发到这里。支持任何
          OpenAI 兼容服务（DeepSeek 默认；换 Kimi/智谱/通义等把地址填成对应官方 API 即可，
          模型名用服务商要求的）。改动后重启应用生效。
        </p>
      </div>

      <div className="panel">
        <h3>展示币种</h3>
        <div className="row">
          <select
            value={settings.display_currency}
            onChange={(e) => setSettingsState({ ...settings, display_currency: e.target.value })}
          >
            <option value="CNY">人民币 (CNY)</option>
            <option value="USD">美元 (USD)</option>
          </select>
          {settings.display_currency === 'CNY' && (
            <label>
              1 USD =
              <input
                type="number"
                step="0.01"
                value={settings.usd_cny_rate}
                onChange={(e) =>
                  setSettingsState({ ...settings, usd_cny_rate: Number(e.target.value) || 7.1 })
                }
              />
              CNY
            </label>
          )}
          <button className="btn primary" onClick={saveSettings}>
            保存设置
          </button>
        </div>
      </div>

      <div className="panel">
        <h3>模型单价库（USD / 百万 Token）</h3>
        <p className="muted">
          内置价格快照基于 2026-09 公开定价，可能随厂商调整；为“估算”费用所用，官方账单金额优先。可新增/覆盖自定义模型。
        </p>
        <table className="data-table price-table">
          <thead>
            <tr>
              <th>厂商</th>
              <th>模型</th>
              <th>输入 $/M</th>
              <th>输出 $/M</th>
              <th>缓存输入 $/M</th>
              <th>来源</th>
            </tr>
          </thead>
          <tbody>
            {prices.map((p) => (
              <tr key={`${p.model_id}-${p.source}`}>
                <td>{p.provider_name}</td>
                <td>{p.model_name}</td>
                <td className="num">{p.input_per_mtok}</td>
                <td className="num">{p.output_per_mtok}</td>
                <td className="num">{p.cached_input_per_mtok ?? '-'}</td>
                <td>{p.source === 'custom' ? '自定义' : '内置'}</td>
              </tr>
            ))}
          </tbody>
        </table>

        <h4 style={{ marginTop: 16 }}>新增 / 覆盖自定义单价</h4>
        <div className="form-grid">
          <input
            placeholder="厂商代码(如 moonshot)"
            value={newPrice.provider_code}
            onChange={(e) => setNewPrice({ ...newPrice, provider_code: e.target.value })}
          />
          <input
            placeholder="厂商名(可选)"
            value={newPrice.provider_name}
            onChange={(e) => setNewPrice({ ...newPrice, provider_name: e.target.value })}
          />
          <input
            placeholder="模型名 *"
            value={newPrice.model_name}
            onChange={(e) => setNewPrice({ ...newPrice, model_name: e.target.value })}
          />
          <input
            placeholder="输入 $/M *"
            type="number"
            value={newPrice.input}
            onChange={(e) => setNewPrice({ ...newPrice, input: e.target.value })}
          />
          <input
            placeholder="输出 $/M *"
            type="number"
            value={newPrice.output}
            onChange={(e) => setNewPrice({ ...newPrice, output: e.target.value })}
          />
          <input
            placeholder="缓存输入 $/M"
            type="number"
            value={newPrice.cached}
            onChange={(e) => setNewPrice({ ...newPrice, cached: e.target.value })}
          />
          <button className="btn primary" onClick={addCustomPrice}>
            保存单价
          </button>
        </div>
      </div>

      <div className="panel">
        <h3>数据维护</h3>
        <div className="row" style={{ gap: 8 }}>
          <button className="btn" disabled={busy} onClick={handleExport}>
            导出全部明细 CSV
          </button>
          <button className="btn" onClick={handleBackup}>
            备份数据库
          </button>
          <button className="btn danger" onClick={handleRestore}>
            从备份恢复…
          </button>
        </div>
        <p className="muted" style={{ marginTop: 8 }}>
          金额换算参考：{fmtCostNumber(100, cc)} ≈ 100 USD · 总Token单位换算使用 K/M 缩写。
        </p>
      </div>
    </div>
  )
}
