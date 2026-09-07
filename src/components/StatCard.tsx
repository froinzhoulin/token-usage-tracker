import type { ReactNode } from 'react'

export interface StatCardProps {
  label: string
  value: ReactNode
  hint?: ReactNode
  accent?: boolean
}

export default function StatCard({ label, value, hint, accent }: StatCardProps) {
  return (
    <div className={`stat-card ${accent ? 'accent' : ''}`}>
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
      {hint && <div className="stat-hint">{hint}</div>}
    </div>
  )
}
