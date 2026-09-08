// 采集端点接入说明页(v0.2 主入口):
// 展示本机上报 URL、curl/Python 示例, 并可手动粘贴一条记录快速验证。

import { useEffect, useState } from 'react'

import { addRecord, collectorStatus } from '../api/client'

interface Status {
  port: number
  started: boolean
  error: string | null
  base_url: string
}

export default function CollectPage() {
  const [status, setStatus] = useState<Status | null>(null)
  const [msg, setMsg] = useState('')
  const [sending, setSending] = useState(false)

  // 手动粘贴表单
  const [model, setModel] = useState('deepseek-v4-flash')
  const [provider, setProvider] = useState('deepseek')
  const [prompt, setPrompt] = useState('')
  const [completion, setCompletion] = useState('')
  const [session, setSession] = useState('')
  const [cost, setCost] = useState('')

  useEffect(() => {
    collectorStatus()
      .then(setStatus)
      .catch(() => {})
  }, [])

  const curlExample = status
    ? `curl -X POST ${status.base_url}/api/v1/usage \\
  -H "Content-Type: application/json" \\
  -d '{"model":"deepseek-v4-flash","provider":"deepseek","prompt_tokens":100,"completion_tokens":50}'`
    : ''

  async function handleSend() {
    setSending(true)
    setMsg('')
    try {
      await addRecord({
        recorded_at: new Date().toISOString(),
        source: 'manual',
        provider_code: provider || null,
        model_name: model || null,
        session_id: session || null,
        prompt_tokens: prompt ? Number(prompt) : null,
        completion_tokens: completion ? Number(completion) : null,
        cost_usd: cost ? Number(cost) : null,
      })
      setMsg('已记入一条用量，去「看板」查看效果')
      setPrompt('')
      setCompletion('')
    } catch (e) {
      setMsg(`失败: ${String(e)}`)
    } finally {
      setSending(false)
    }
  }

  return (
    <div className="page">
      <div className="page-header">
        <h2>用量上报</h2>
        <span className={`badge ${status?.started ? 'ok-badge' : ''}`}>
          {status?.started
            ? `收集服务运行中 · ${status.base_url}`
            : `收集服务未启动 ${status?.error ? `(${status.error})` : ''}`}
        </span>
      </div>

      <div className="panel highlight-panel">
        <h3>接入方式（推荐，自动记录）</h3>
        <p>
          在本机程序调用大模型返回后，把用量 POST 到下面的地址即可，应用会自动入库并在看板累计。
          <strong>只需要能区分模型名和 token 数，响应里的 usage 字段就能直接填。</strong>
        </p>
        <div className="endpoint-box">
          <code>
            {status?.base_url || 'http://127.0.0.1:8765'}
            /api/v1/usage
          </code>
        </div>
        <h4 style={{ margin: '14px 0 6px' }}>curl 示例</h4>
        <pre className="code-block">{curlExample || '等待收集服务…'}</pre>
        <details style={{ marginTop: 8 }}>
          <summary>看 Python 示例</summary>
          <pre className="code-block">{`import requests
resp = requests.post(  # 你在自己程序里调用模型后
    "http://127.0.0.1:8765/api/v1/usage",
    json={
        "model": "deepseek-v4-flash",  # 或你实际使用的模型名(deepseek-v4-pro 等)
        "provider": "deepseek",
        "prompt_tokens": 1234,      # resp.usage.prompt_tokens
        "completion_tokens": 567,   # resp.usage.completion_tokens
        "session_id": "s-001",      # 可选
        "request_id": "r-001",      # 可选, 防重复
    },
)`}</pre>
        </details>
        <p className="muted" style={{ fontSize: 12 }}>
          字段说明：model 必填（如 deepseek-v4-flash）；prompt_tokens/completion_tokens 尽量给；
          cost_cny 或 cost_usd 是官方账单金额（给了就不估算）；request_id 相同会自动去重。
        </p>
      </div>

      <div className="panel">
        <h3>手动记一条（快速验证）</h3>
        <div className="form-grid">
          <input
            placeholder="模型 (必填, 如 deepseek-v4-flash)"
            value={model}
            onChange={(e) => setModel(e.target.value)}
          />
          <input
            placeholder="厂商 (如 deepseek/kimi/glm)"
            value={provider}
            onChange={(e) => setProvider(e.target.value)}
          />
          <input
            placeholder="输入 token"
            type="number"
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
          />
          <input
            placeholder="输出 token"
            type="number"
            value={completion}
            onChange={(e) => setCompletion(e.target.value)}
          />
          <input
            placeholder="会话ID (可选)"
            value={session}
            onChange={(e) => setSession(e.target.value)}
          />
          <input
            placeholder="费用USD (可选, 留空按单价估)"
            type="number"
            value={cost}
            onChange={(e) => setCost(e.target.value)}
          />
          <button className="btn primary" disabled={sending || !model.trim()} onClick={handleSend}>
            {sending ? '写入中…' : '记一条'}
          </button>
        </div>
        {msg && <p className={msg.startsWith('失败') ? 'error-box' : 'ok-box'}>{msg}</p>}
      </div>
    </div>
  )
}
