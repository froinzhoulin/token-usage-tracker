import { useEffect, useState } from 'react'
import { getHealth, inTauri, type HealthInfo } from './api/client'

function StatusRow({ label, value, ok }: { label: string; value: string; ok: boolean }) {
  return (
    <div className={`status-row ${ok ? 'ok' : 'warn'}`}>
      <span className="status-label">{label}</span>
      <span className="status-value">{value}</span>
    </div>
  )
}

export default function App() {
  const [health, setHealth] = useState<HealthInfo | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    getHealth()
      .then(setHealth)
      .catch((e) => setError(String(e)))
  }, [])

  return (
    <div className="app">
      <header className="topbar">
        <h1>Token Usage Tracker</h1>
        <span className="badge">{health?.app_version ?? '...'}</span>
      </header>

      <main className="content">
        <section className="panel">
          <h2>M0 原型冒烟</h2>
          <p>
            应用壳已启动。下方展示 Tauri 后端（Rust）健康状态。当前运行环境：
            <strong>{inTauri() ? 'Tauri 桌面' : '浏览器预览'}</strong>。
          </p>

          {error && <div className="error-box">调用后端失败: {error}</div>}

          {health ? (
            <div className="status-list">
              <StatusRow
                label="后端进程"
                value={health.db_ready ? '已连接' : '未连接(浏览器预览)'}
                ok={health.db_ready}
              />
              <StatusRow
                label="SQLite 数据库"
                value={health.db_ready ? health.db_path! : '未初始化'}
                ok={health.db_ready}
              />
              <StatusRow
                label="Schema 版本"
                value={health.db_ready ? String(health.db_version) : '-'}
                ok={health.db_ready}
              />
              <StatusRow label="消息" value={health.message} ok={health.db_ready} />
            </div>
          ) : (
            !error && <p className="loading">正在连接后端…</p>
          )}
        </section>

        <section className="panel muted">
          <h2>规划中的模块</h2>
          <ul>
            <li>看板：汇总指标 / 趋势 / 维度分布（M1）</li>
            <li>明细：分页查询 / 编辑 / 标签（M1）</li>
            <li>导入：CSV / JSON 字段映射（M1）</li>
            <li>成本：内置单价库 + 自动换算（M1）</li>
            <li>预算与告警 / 实时采集 / 报表导出（M2）</li>
          </ul>
        </section>
      </main>
    </div>
  )
}
