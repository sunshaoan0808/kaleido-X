# 万象 Kaleido · 部署拓扑（写实版 D3/P-09）

> 更新：2026-08-24。本文档描述**实际运行**的拓扑与端口，全部来自代码证据
> （引用处为 `crates/kaleido-server/src` 实测行），不含"计划中/理想中"的组件。
> 旧的 `DEPLOY_LOG.md` 是逐次部署流水账，本文是静态真相。

## 1. 进程与端口总览

```
                        ┌────────────────────────────────────────────┐
 用户浏览器/移动端 ──→ :18766  kaleido-server (Rust/axum, systemd)   │
                        │   web 静态壳 /api/v1/* SSE 流              │
                        └───┬──────────┬──────────┬──────────┬──────┘
                            │          │          │          │
             :20145 embed  │  :18998  │  :4001   │  :8020   │ LLM_BASE_URL
             proxy(Python) │  nyx-    │  cf-     │  grok2api│ (外部网关,
             BGE-small-zh  │  proxy   │  manager │  (chenyme│ 深度求索等)
                           ▼          ▼          ▼          ▼
```

| 端口 | 组件 | 归属 | 用途 | 代码证据 |
|---|---|---|---|---|
| **:18766** | kaleido-server | 本仓库 `target/release/kaleido-server` | 主服务：web 壳、API、SSE、酒馆回合 | `kaleido-server.service` `KALEIDO_PORT=18766` |
| **:20145** | kaleido-embed-proxy | `<REPO>/scripts/embed-proxy.py`（Python venv） | BGE-small-zh-v1.5 向量（语义搜索兜底） | `kaleido-embed-proxy.service`；`main.rs:890 EMBEDDING_BASE_URL 默认 http://127.0.0.1:20145` |
| 内嵌 fastembed | 同主进程 | `embed_local.rs` | **PRIMARY** 嵌入通道（`KALEIDO_EMBED_INLINE=1` + ONNX 缓存）；proxy 仅 FALLBACK | service 注释 ST-21/22；`Wants=` 而非 `Requires=` |
| **:18998** | nyx-proxy（外部，非本仓库） | 宿主机独立进程 | cogview-4 文生图默认渠道（uniapi） | `main.rs:891 IMAGE_BASE_URL 默认 http://127.0.0.1:18998/v1`；`config.rs:78` |
| **:4001** | cf-manager（外部） | 宿主机独立进程 | flux-1-schnell 免费池（cf-manager 渠道，账号池轮换） | `main.rs:894 CF_IMAGE_BASE_URL 默认 http://127.0.0.1:4001/v1`；`kaleido_tools.rs:94-99` |
| **:8020** | chenyme-grok2api（外部） | Docker 宿主映射 | grok-imagine-image 渠道（必需 aspect_ratio 参数） | `kaleido_tools.rs:137-141 GROK2API_IMAGE_BASE_URL 默认 http://127.0.0.1:8020/v1` |

## 2. 关键链路细节

### 2.1 嵌入（语义搜索）
- 主路径：进程内 fastembed（BGE-small-zh-v1.5, 512 维）。模型目录优先
  `$KALEIDO_EMBED_CACHE`（service 设为 `data/embed-cache`），避免运行时拉 HuggingFace。
- 兜底：本地初始化失败 → 远程 `EMBEDDING_BASE_URL`(:20145)。
- 启动日志 `embed_local warm failed (remote fallback remains)` 属预期降级提示，
  不代表服务不可用。

### 2.2 生图三渠道（`POST /api/v1/kaleido-tools/image`）
- channel 由请求体指定，默认 `uniapi`：
  - `uniapi` → nyx-proxy **:18998** `/v1/images/generations`（cogview-4，免费实测秒回）
  - `cf-manager` → **:4001** flux-1-schnell（返回 url 或 b64_json）
  - `grok2api` → **:8020** grok-imagine-image（**必须**带 `aspect_ratio`）
- **端口重写坑**：chenyme 返回的图片 URL 是容器内端口 **:8000**，宿主映射为
  **:8020**。服务端会做 `:8000→:8020` 重写保证前端可达（`kaleido_tools.rs:170`）；
  下载侧 `download_bytes()` 反向再准备一份 `:8020→:8000` 候选做双试
  （`image_pipeline.rs:272-277`）。

### 2.3 回环调用必须绕代理（no_proxy）
- 图片字节下载 client 显式 `.no_proxy()`（`image_pipeline.rs:274`）。
- **运维含义**：宿主机若设了全局 `http_proxy/https_proxy`，所有
  `127.0.0.1:*` 回环调用（embed proxy / nyx / cf-manager / grok2api / LLM 网关）
  都必须加入 `no_proxy=localhost,127.0.0.1` 或等价配置，否则请求会被
  代理劫持导致连接失败或绕外网一圈。
- systemd 单元内未设代理变量时不受影响；`.env`（EnvironmentFile）里也
  不要放 proxy 变量。

### 2.4 LLM 网关
- `LLM_BASE_URL / LLM_API_KEY / LLM_MODEL` 来自
  `<HOME>/.env` 或项目 `.env`（EnvironmentFile，chmod 600）。
- 管理端「托管 provider」优先于 env（`resolve_llm` P5 逻辑）。

## 3. 数据布局

```
$KALEIDO_DATA (= …/kaleido-server/data)
├── state/users.json        # admin bootstrap（KALEIDO_ADMIN_USER/PASSWORD）
├── config/moa-store.json   # MoA 对比面板持久化
├── graph.sqlite            # 关系图存储
├── plot.sqlite             # foreshadow/analysis/ai_admin/scene_cards 四库共文件
├── search.sqlite           # 语义检索索引 ⚠️ ~57MB，GitHub push 有 >50MB 告警
└── embed-cache/            # fastembed ONNX 快照
```
改名兼容（60c35b6 起）：新布局用 `<root>/Kaleido/…`；发现旧
`<root>/MuseAI/…` 时自动回退读取（`kaleido_core::data_dir`），环境变量
`KALEIDO_*` 未设时回退 `MUSEAI_*`（compat_env + main.rs 启动别名播种）。

## 4. 服务管理

```bash
./scripts/kaleido-deploy.sh install   # 安装并启用两个 systemd 单元
./scripts/kaleido-deploy.sh status    # 双服务状态
./scripts/kaleido-deploy.sh restart   # 重启 + 双健康检查（20145/health, 18766/health）
./scripts/kaleido-deploy.sh logs      # journalctl -u kaleido-embed-proxy -u kaleido-server -f
./scripts/kaleido-deploy.sh uninstall # 移除单元
```

冒烟验证清单（不碰 prod 时在隔离端口演练）：
```bash
curl -s http://127.0.0.1:18766/healthz
curl -s -X POST …/api/v1/auth/login -d '{"username":"admin","password":"***"}'
# SSE 错误事件自 D2 起携带稳定 code（UPSTREAM_CONNECT 等）
```

## 5. 已知注意项

1. **search.sqlite 57MB** — GitHub 会警告 >50MB（仅 warning，push 成功）。
   后续可选 git-lfs 或 .gitignore + 部署期生成。
2. **WorkingDirectory 即项目根** — asr worker 以相对路径 `.venv-asr/bin/python`
   探测（asr.rs 编译期回退已内置双候选）。
3. **PrivateTmp=yes** — /tmp/fastembed_cache 被隔离，模型快照必须放在
   KALEIDO_DATA 下，不能依赖共享 /tmp。
4. **embed-proxy Wants= 非 Requires=** — proxy 挂了不阻塞主服务启动
   （inline 主通道仍在）。
