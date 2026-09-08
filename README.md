# Token Usage Tracker

> 本地运行的 LLM Token 用量统计与分析桌面工具。
> 自动检测 DeepSeek Harness 用量 · 本地透明代理采集 · 数据 100% 存本机，不上传任何东西。

![Tauri 2](https://img.shields.io/badge/Tauri-2.x-24C8D8?logo=tauri&logoColor=white)
![React 18](https://img.shields.io/badge/React-18-61DAFB?logo=react&logoColor=black)
![Rust](https://img.shields.io/badge/Rust-stable-DEA584?logo=rust&logoColor=white)
![SQLite](https://img.shields.io/badge/SQLite-WAL-003B57?logo=sqlite&logoColor=white)
![Windows](https://img.shields.io/badge/Windows-10%2B-0078D6?logo=windows&logoColor=white)

LLM 用量分散在多次会话、多个客户端里，说不清**花了多少、花在哪、是否失控**。本工具把本机所有模型调用自动汇聚成一本账：Token、费用、趋势、分布，全部本地完成——无需账号、无需联网、不存 API Key。

## 功能特性

**数据采集（三条通道，可叠加使用）**

- 🔍 **DSH 自动检测**（零配置）：应用启动后自动读取 DeepSeek Harness 的会话用量快照，每 3 秒增量入库，模型/厂商/会话/缓存自动识别，重启不重复计数
- 🔁 **本地透明代理**：把任意 OpenAI 兼容客户端的 `base_url` 改成本机地址即可，自动转发 + 自动解析（流式/非流式）响应里的 usage；API Key 只透传、不读取、不落盘
- 📮 **HTTP 上报端点**：`POST /api/v1/usage`，供脚本/程序随时上报；另有手动表单录入兜底
- ✅ 按 `request_id` 去重，同一调用永远不会被统计两次

**统计与成本**

- 📊 看板：今日 24 小时柱状图 / 按日 Token+费用双轴趋势 / 模型用量排行 / 最近检测实时流，5 秒自动刷新
- 💰 内置价格库（DeepSeek / OpenAI / Anthropic / Kimi，USD per 百万 Token），支持自定义单价覆盖与模型别名映射（如 `deepseek-chat` → `deepseek-v4-flash`）
- 🏷 费用优先级：官方账单金额 > 单价自动换算（明细中标注「估算」）；展示币种 CNY / USD 可切换（手动汇率）
- 📋 明细：分页 / 多条件筛选 / 行内编辑 / 删除；导出 CSV（UTF-8 BOM，Excel 友好）/ JSON；数据库一键备份与恢复

## 数据接入方式

### 1) DSH 自动检测（推荐，零配置）

本机存在 `~/.dsh` 时自动开始：轮询 `~/.dsh/storages/session_projcache/sessions/*.json`，以 `(turn, step)` 水位线识别每一次新调用。入库口径：`prompt = 未缓存输入 + 缓存命中`，缓存 Token 单列以便分开计价。你在 DSH 里正常对话即可，无需任何改动。

### 2) 本地透明代理

在你的程序里只改一行 API 地址（模型名和 Key 保持不变）：

```python
client = OpenAI(
    api_key="sk-你的key",
    base_url="http://127.0.0.1:8765",   # ← 只改这一行
)
```

- 流式请求自动注入 `stream_options.include_usage=true`，打字机体验不受影响
- 上游地址可在设置页更换（Kimi / 智谱 / 通义等任何 OpenAI 兼容服务）

### 3) HTTP 上报端点

```bash
curl -X POST http://127.0.0.1:8765/api/v1/usage \
  -H "Content-Type: application/json" \
  -d '{"model_name":"deepseek-v4-flash","prompt_tokens":1234,"completion_tokens":567,"cost_cny":0.02}'
```

- `model_name` 必填；`request_id` 提供则自动去重；`cost_cny` / `cost_usd` 提供则优先采用，否则按价格库估算
- `GET /api/v1/ping` 用于连通性测试
- 服务只绑定 `127.0.0.1`，外部不可达

## 快速开始

系统要求：Windows 10/11（含 WebView2 运行时）。

1. 下载安装包 `Token Usage Tracker_0.1.0_x64-setup.exe`（或直接运行绿色版 `token-usage-tracker.exe`）
2. 打开应用即可——本机有 `~/.dsh` 时自动检测即刻生效，到「看板」页等待数据点亮

### 从源码构建

依赖：Node.js ≥ 18、Rust stable（MSVC 工具链）、WebView2 Runtime。

```bash
npm install
npm run tauri dev      # 开发调试
npm run tauri build    # 产出安装包
# → src-tauri/target/release/bundle/nsis/*.exe
# → src-tauri/target/release/bundle/msi/*.msi
```

### 运行测试

```bash
cd src-tauri
cargo test --lib                                                        # 19 项单元测试
cargo test --test integration                                           # 导入-统计-导出全链路
cargo test --release --test integration perf_100k -- --ignored --nocapture   # 10 万行性能验收
```

## 项目结构

```
token-usage-tracker/
├─ src/                     # React 前端
│  ├─ api/client.ts         #   Tauri IPC 封装 + 类型镜像
│  ├─ pages/                #   看板 / 明细 / 上报 / 设置
│  └─ components/ utils/ styles/
├─ src-tauri/
│  ├─ src/commands/         # IPC 命令薄壳
│  ├─ src/domain/           # 业务规则（记录/价格/统计/导入导出）
│  ├─ src/db/               # SQLite 连接与增量迁移
│  ├─ src/collector.rs      # 127.0.0.1 HTTP 收集端点
│  ├─ src/proxy.rs          # OpenAI 兼容透明代理
│  ├─ src/dsh_watcher.rs    # DSH 快照水位线检测
│  └─ tests/integration.rs  # 端到端集成测试
└─ scripts/                 # 图标生成等辅助脚本
```

## 数据与隐私

- 所有数据存于本地 SQLite（WAL 模式）：`%APPDATA%\com.tokentracker.app\tracker.db`，设置页可一键备份/恢复
- **不采集、不上传任何数据**，核心功能完全离线可用
- **不读取、不存储 API Key**（代理仅透传）
- 内置价格为公开定价快照（可能滞后于厂商调价），设置页可自定义覆盖；标「估算」的费用均为换算值

## 已知限制

- DSH 快照格式若随 DSH 版本升级变化，解析逻辑需同步更新
- 应用未运行期间的多次调用，只能补记每会话最后一次（快照仅含最近一次调用的用量）
- CSV 导入界面暂未开放（后端解析已实现并测试覆盖，计划在后续版本回归）

## 路线图

- [ ] 预算设置与阈值告警（应用内 / 系统通知）
- [ ] CSV 导入界面回归 + Excel 导出
- [ ] 界面国际化（i18n）
- [ ] macOS / Linux 打包

## 示例数据

- [示例数据](sample-usage.csv) — 用量记录格式样例

## License

尚未设置开源协议（个人项目可考虑 MIT / Apache-2.0）。
