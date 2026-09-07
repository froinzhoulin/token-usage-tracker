/** 数字/金额格式化与币种换算助手 */

export interface CurrencyContext {
  display: 'CNY' | 'USD'
  usdCnyRate: number
}

export function fmtTokens(n: number | null | undefined): string {
  if (n === null || n === undefined) return '-'
  if (Math.abs(n) >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`
  if (Math.abs(n) >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return String(n)
}

/** USD → 展示币种 */
export function displayCost(usd: number | null | undefined, cc: CurrencyContext): string {
  if (usd === null || usd === undefined) return '-'
  if (cc.display === 'USD') {
    return `$${usd.toFixed(4)}`
  }
  const cny = usd * cc.usdCnyRate
  return `¥${cny.toFixed(2)}`
}

export function costNumber(usd: number | null | undefined, cc: CurrencyContext): number {
  if (usd === null || usd === undefined) return 0
  return cc.display === 'USD' ? usd : usd * cc.usdCnyRate
}

export function fmtCostNumber(v: number, cc: CurrencyContext): string {
  if (cc.display === 'USD') return `$${v.toFixed(4)}`
  return `¥${v.toFixed(2)}`
}

export function currencySymbol(cc: CurrencyContext): string {
  return cc.display === 'USD' ? '$' : '¥'
}

export function fmtDate(iso: string): string {
  // ISO UTC → 本地 YYYY-MM-DD HH:mm
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  const pad = (x: number) => String(x).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

export function fmtDay(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso.slice(0, 10)
  const pad = (x: number) => String(x).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}
