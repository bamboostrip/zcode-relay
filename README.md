# zcode-relay (Rust 版)

把 **ZCode Coding Plan 的额度**（GLM-5.2 / GLM-5-Turbo，含非高峰期 0.67x 折算）
转发成一个**标准 API 服务**，让其他工具、其他电脑也能用。

**Rust 实现**：axum + reqwest + tokio。静态 musl 二进制，运行内存 ~3-15MB
（Python 版 ~50-90MB），适合内存紧张的服务器。

## 功能（与 Python 版 1:1 对齐）

- `POST /v1/messages` Anthropic 格式（Claude Code / Cline-Anthropic）
- `POST /v1/chat/completions` OpenAI 格式（通用工具）
- `GET /v1/models` 模型列表（config 权威清单 ∪ 上游实时拉取，自动写回）
- `GET /healthz` 健康检查
- ZCode 身份 header 注入（对齐真实抓包，走 Coding Plan 额度）
- 重试退避（429/5xx，指数退避 2→20s，10 次，对齐 ZCode「10 次机会」）
- SSE 流式透传（HTTP/2 连接池复用）
- body 纯透传（thinking/effort/max 等字段完整到达智谱）
- 管理鉴权（management_key）

## 工作原理

```
[你的工具 / 其他电脑]
        │  POST /v1/messages 或 /v1/chat/completions（带 management_key）
        ▼
[ zcode-relay ]  ← 注入 ZCode 身份 header + plan key + 重试
        │
        ▼
[ 智谱 open.bigmodel.cn ]  → 识别为 ZCode 来源，走 plan 额度
```

## 快速开始

### 方式 A：本地编译运行
```bash
cargo build --release
./target/release/zcode-relay
```
（需 Rust 工具链，首次编译拉依赖较慢）

### 方式 B：Docker 部署（推荐服务器）
```bash
cp config.example.json config.json
$EDITOR config.json     # 填 api_key、management_key
docker compose up -d --build
docker compose logs -f
```

要点：
- `config.json` 可写挂载（服务会写回合并后的模型清单），改完 `docker compose restart` 生效。
- 镜像基于 distroless（无 shell），运行内存 ~3-15MB，`mem_limit: 64m`。

## 配置说明

| 字段 | 说明 | 默认 |
|------|------|------|
| `api_key` | Coding Plan 的 API key，或填 `"auto"` 自动读本机 ZCode 安装目录的 key | 必填 |
| `management_key` | 外部调用方鉴权 key | 强烈建议设 |
| `host` / `port` | 监听地址 | `0.0.0.0` / `8787` |
| `anthropic_base` / `openai_base` | 智谱上游端点 | 已填好 |
| `zcode_app_version` 等 | ZCode 身份字段 | `3.1.0` |
| `models` | 权威清单，与上游并集去重后写回 | `GLM-5.2, glm-5-turbo` |
| `upstream_timeout` | 上游超时秒数 | `300` |

## 各工具接入

假设 relay 跑在 `192.168.1.206:8787`，management_key = `sk-relay-xxx`。

**OpenAI 兼容工具**（Cline / Continue / 自研）：
- Base URL: `http://192.168.1.206:8787/v1`
- API Key: `sk-relay-xxx`
- Model: `GLM-5.2`

**Anthropic 工具**（Claude Code / Cline-Anthropic）：
- Base URL: `http://192.168.1.206:8787`
- API Key: `sk-relay-xxx`

## 项目结构

```
zcode-relay-rust/
├── src/
│   ├── main.rs     启动入口（配置加载 + uvicorn/axum serve）
│   ├── app.rs      axum 路由 + 鉴权 + 端点
│   ├── proxy.rs    reqwest 上游转发 + 重试 + SSE（核心）
│   ├── headers.rs  ZCode 身份 header 构建
│   ├── retry.rs    重试退避逻辑
│   ├── auth.rs     管理鉴权
│   ├── config.rs   配置加载 + 写回
│   └── models.rs   模型清单（config ∪ 上游）
├── Cargo.toml
├── Dockerfile      静态 musl + distroless
├── docker-compose.yml
└── config.example.json
```

## 开发

```bash
cargo test          # 跑所有单元测试
cargo build         # 调试构建
cargo build --release  # 发布构建（LTO + strip，最小体积）
```
