# Kaleido

Kaleido is a self-hosted **Story Tavern + Chat** server: a Rust (axum) backend
plus a browser SPA for writing interactive fiction with characters, world books,
works management, and a self-evolving "harness" that can propose and apply
refinements to its own prompts and memory.

## Features

- **Story Tavern** — characters, world books, lore, session saves, and playable
  story threads.
- **Chat** — streaming (SSE) chat with dual-agent orchestration, context
  compaction, and memory weaving.
- **Works** — a jailed filesystem API for drafts, world books, and assets.
- **Jobs v2** — queued asynchronous jobs with streaming progress/events.
- **Self-evolution harness** — LLM-driven refinement proposals with guidance
  anchoring, apply/rollback, and audit history.
- **Embeddings** — semantic search (inline fastembed, or a sidecar proxy).
- **Book export** — EPUB and PDF generation (native, no external binaries).
- **Living characters** — pockets & wardrobe, six-dimension needs with decay
  and catastrophe, bond/trust dynamics, growth rings, promises with
  accountability, mood baseline, presence derivation
  ([Front Porch AI](https://github.com/linux4life1/front-porch-AI)-inspired,
  reimplemented — see [`docs/ATTRIBUTIONS.md`](docs/ATTRIBUTIONS.md#14-front-porch-ai--linux4life1front-porch-aiagpl-30重实现未搬代码)).
- **Auto event extraction** — end-of-turn background LLM writes pocket ops,
  promises, growth, bond, and journal cards automatically (toggleable per
  session, small model, fail-open).
- **Worldbook timed effects** — sticky/cooldown lore entries wired into the
  Tavern prompt pipeline with director-console pills.

## Layout

```
kaleido/
  crates/kaleido-core/     # DataRoot, Auth, Jobs, Sessions, AppState, PartnerStore
  crates/kaleido-server/   # axum HTTP + chat SSE + partner/settings + /web
  crates/kaleido-harness/  # self-evolution harness
  src/                     # frontend source (ESM, built with Vite)
  web/                     # SPA + build assets
  desktop-tauri/           # thin Tauri 2 desktop shell (optional)
  docker/                  # Caddyfile for TLS
  docs/                    # contracts, deploy, hardening, decisions
```

## Quick start (host)

```bash
cp .env.example .env          # edit passwords + LLM_*
export $(grep -v '^#' .env | xargs)
cargo run -p kaleido-server --release
# → http://127.0.0.1:18766/health
# → http://127.0.0.1:18766/web/
```

The admin user is bootstrapped automatically on first start (or refused, with a
clear error, if `KALEIDO_ADMIN_PASSWORD` is unset).

## Build the frontend

```bash
npm install
npm run build:vite          # bundles src/ → web/assets/
```

## Docker

```bash
docker compose up -d --build          # server on 127.0.0.1:18766
docker compose --profile tls up -d    # + Caddy (edit domain in docker/Caddyfile)
```

## Configuration

See `.env.example` for the full list. All environment variables use the
`KALEIDO_*` prefix (legacy `MUSEAI_*` aliases are still accepted for
compatibility).

## API

The API surface (auth, sessions, jobs, partner, works, harness) is documented in
[`docs/contracts/`](docs/contracts/) and [`docs/API_CONTRACT.md`](docs/API_CONTRACT.md).

## Deploying

See [`docs/DEPLOY.md`](docs/DEPLOY.md) and [`docs/HARDENING.md`](docs/HARDENING.md).
Systemd unit files are provided under `scripts/` (`kaleido-server.service`,
`kaleido-embed-proxy.service`).

## Docs

- [`docs/DECISIONS.md`](docs/DECISIONS.md) — architecture decisions.
- [`docs/ATTRIBUTIONS.md`](docs/ATTRIBUTIONS.md) — upstream sources and credits.
- [`docs/contracts/`](docs/contracts/) — HTTP/SSE/error contracts.

## License

MIT. See the `docs/ATTRIBUTIONS.md` for third-party credits.