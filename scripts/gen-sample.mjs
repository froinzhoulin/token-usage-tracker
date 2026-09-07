// 生成样例用量 CSV, 覆盖 deepseek/openai/anthropic 三家、多模型多会话,
// 日期落在最近 ~20 天内, 部分行带 cost(模拟官方账单), 部分留空(演示自动换算)。
import { writeFileSync } from 'node:fs'
import { resolve } from 'node:path'

const outPath = resolve(process.argv[2] ?? 'sample-usage.csv')

const PROVIDERS = [
  { provider: 'deepseek', models: ['deepseek-v4-flash', 'deepseek-v4-pro'] },
  { provider: 'openai', models: ['gpt-5.6-sol', 'gpt-5.6-luna', 'gpt-5.5'] },
  { provider: 'anthropic', models: ['claude-opus-5', 'claude-sonnet-5'] },
]

const SESSIONS = ['chat', 'coding', 'research', 'data-analysis', 'writing']
const PROJECTS = ['tut-tracker', 'website', 'docs', '']
const TAGS = [['dev'], ['research'], [''], ['daily'], ['arch']]

const rand = (min, max) => Math.floor(Math.random() * (max - min + 1)) + min
const pick = (arr) => arr[rand(0, arr.length - 1)]

const rows = []
const today = new Date()
const dayMs = 24 * 3600 * 1000

for (let i = 0; i < 220; i++) {
  const p = pick(PROVIDERS)
  const model = pick(p.models)
  const dayOffset = rand(0, 19)
  const d = new Date(today.getTime() - dayOffset * dayMs)
  d.setHours(rand(0, 23), rand(0, 59), 0, 0)
  const isFlash = model.includes('flash') || model.includes('luna') || model.includes('sonnet')
  const prompt = isFlash ? rand(2_000, 180_000) : rand(5_000, 320_000)
  const completion = Math.floor(prompt * rand(20, 90) / 100)
  const cached = Math.floor(prompt * rand(0, 40) / 100)
  const hasCost = Math.random() < 0.55
  const dt = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}:00`
  rows.push([
    dt,
    model,
    p.provider,
    pick(SESSIONS),
    `req-${p.provider}-${rand(100000, 999999)}`,
    String(prompt),
    String(completion),
    String(cached),
    hasCost ? (Math.random() * 8).toFixed(6) : '',
    pick(PROJECTS),
    pick(TAGS).join(';'),
  ].join(','))
}

const header = 'recorded_at,model_name,provider_code,session_id,request_id,prompt_tokens,completion_tokens,cached_tokens,cost_usd,project,tags'
writeFileSync(outPath, header + '\n' + rows.join('\n') + '\n', 'utf8')
console.log(`sample written: ${outPath} (${rows.length} rows)`)
