// 接入页(v0.4): 主推"本地透明代理"自动检测 ——
// 把程序/客户端的 API 地址指向本机即可, 工具自动转发+自动记用量。

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

  // 手动上报表单(备用)
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

  const proxyUrl = status ? `${status.base_url}` : 'http://127.0.0.1:8765'

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
        <h2>自动检测接入</h2>
        <span className={`badge ${status?.started ? 'ok-badge' : ''}`}>
          {status?.started
            ? `服务运行中 · ${status.base_url}`
            : `服务未启动 ${status?.error ? `(${status.error})` : ''}`}
        </span>
      </div>

      <div className="panel highlight-panel">
        <h3>⭐ 全自动：检测本机 DSH（DeepSeek Harness）用量</h3>
        <p>
          工具已在自动检测这台机器上 <strong>DeepSeek Harness</strong> 的每一次模型调用
          （读取 <code>~/.dsh</code> 会话用量快照，每 3 秒刷新）。你在 DSH 里正常对话即可，
          <strong>无需任何配置或代码改动</strong>——模型、token、会话自动识别并计入看板。
        </p>
        <div className="endpoint-box">
          <code>自动检测运行中 · 数据来源 ~/.dsh/storages/session_projcache</code>
        </div>
        <p className="muted" style={{ marginTop: 8 }}>
          每次 DSH 调用（如 deepseek-v4-flash）都会自动记一条：模型名、输入/输出/缓存 token、
          会话、估算费用。若本机还运行着其它国产模型客户端，可改用下方「透明代理」方式接入。
        </p>
      </div>

      <details className="panel">
        <summary style={{ cursor: 'pointer', fontWeight: 600 }}>
          透明代理（可选：检测非 DSH 的本机程序，需把 base_url 指向本机）
        </summary>
        <div style={{ marginTop: 10 }}>
          <p>
            在你调用 DeepSeek 的程序/客户端里，把 API 地址（base_url）改成下面这个地址，
            <strong>模型名和 Key 保持不变</strong>。之后的每一次调用都会自动识别模型、自动记录用量：
          </p>
          <div className="endpoint-box">
            <code>{proxyUrl}</code>
          </div>
          <table className="mini-table proxy-compare">
            <tbody>
              <tr>
                <td>改之前</td>
                <td>
                  <code>base_url = https://api.deepseek.com</code>
                </td>
              </tr>
              <tr>
                <td>改之后</td>
                <td>
                  <code>base_url = {proxyUrl}</code>
                </td>
              </tr>
            </tbody>
          </table>
          <p className="muted" style={{ marginTop: 8 }}>
            原理：工具在本机监听 OpenAI 兼容接口，把 /chat/completions 请求转发给真实 DeepSeek
            服务（默认上游，可在设置修改），同时自动解析响应里的 usage 入库。你的 API Key
            只是路过转发，<strong>不会被读取或保存</strong>。
          </p>
          <h4 style={{ margin: '14px 0 6px' }}>Python 示例（改一行即可）</h4>
          <pre className="code-block">{`from openai import OpenAI

client = OpenAI(
    api_key="sk-你的key",          # 保持不变
    base_url="${proxyUrl}",   # ← 只改这一行
)

resp = client.chat.completions.create(
    model="deepseek-v4-flash",     # 你的模型名, 保持不变
    messages=[{"role": "user", "content": "你好"}],
)
print(resp.choices[0].message.content)`}</pre>
        </div>
      </details>

      <div className="panel">
        <h3>手动记一条（备用 / 验证用）</h3>
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
