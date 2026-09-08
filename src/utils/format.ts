/** 数字/金额格式化与币种换算助手 */

export interface CurrencyContext {
  display: 'CNY' | 'USD'
  usdCnyRate: number
}

/** token 数显示: 完整数字(千位分隔), 不用 K/M 缩写 */
export function fmtTokens(n: number | null | undefined): string {
  if (n === null || n === undefined) return '-'
  return n.toLocaleString('en-US')
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

/** 本地时区日期串 YYYY-MM-DD(供后端按本地日过滤, 后端转 UTC) */
export function localDateStr(d: Date): string {
  const pad = (x: number) => String(x).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

/** 今天(本地)往前 n 天的日期串 */
export function daysAgoLocal(n: number): string {
  const d = new Date()
  d.setDate(d.getDate() - n)
  return localDateStr(d)
}

/** 今天(本地)日期串 */
export function todayLocal(): string {
  return localDateStr(new Date())
}

/** 本地 HH:MM:SS */
export function fmtClock(d: Date): string {
  const pad = (x: number) => String(x).padStart(2, '0')
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
}
