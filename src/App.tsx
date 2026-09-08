import { useEffect, useState } from 'react'
import { getHealth, inTauri, type HealthInfo } from './api/client'
import Dashboard from './pages/Dashboard'
import Records from './pages/Records'
import CollectPage from './pages/CollectPage'
import SettingsPage from './pages/Settings'
import './styles/global.css'

type Tab = 'dashboard' | 'records' | 'collect' | 'settings'

const TABS: { key: Tab; label: string }[] = [
  { key: 'dashboard', label: '看板' },
  { key: 'records', label: '明细' },
  { key: 'collect', label: '上报' },
  { key: 'settings', label: '设置' },
]

export default function App() {
  const [tab, setTab] = useState<Tab>('dashboard')
  const [health, setHealth] = useState<HealthInfo | null>(null)

  useEffect(() => {
    getHealth()
      .then(setHealth)
      .catch(() => {})
  }, [])

  return (
    <div className="app">
      <header className="topbar">
        <h1>Token Usage Tracker</h1>
        <span className="badge" title={health?.db_path ?? ''}>
          {health?.app_version ?? '...'}
          {!inTauri() && ' · 浏览器预览'}
        </span>
      </header>

      <nav className="navbar">
        {TABS.map((t) => (
          <button
            key={t.key}
            className={`nav-btn ${tab === t.key ? 'active' : ''}`}
            onClick={() => setTab(t.key)}
          >
            {t.label}
          </button>
        ))}
      </nav>

      <main className="content">
        {tab === 'dashboard' && <Dashboard />}
        {tab === 'records' && <Records />}
        {tab === 'collect' && <CollectPage />}
        {tab === 'settings' && <SettingsPage />}
      </main>

      {health && !health.db_ready && inTauri() && (
        <footer className="warn-bar">后端未就绪: {health.message}</footer>
      )}
    </div>
  )
}
