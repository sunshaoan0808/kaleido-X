# Kaleido Desktop (thin Tauri 2 shell)

**Decision (D5):** keep desktop Tauri; **same** `kaleido-server` HTTP/SSE as Web/Android — no re-embedded business commands.

## What this is

- Tauri 2 WebView window titled `Kaleido · 沉浸叙事工作台`
- On launch, navigates to the workbench URL
- Two tiny invoke helpers only: `get_api_base`, `get_start_url`

## Env

| Var | Default | Meaning |
|-----|---------|---------|
| `KALEIDO_DESKTOP_URL` | `http://127.0.0.1:18766/web/?v=desktop` | Page to load |
| `KALEIDO_API_BASE` | `http://127.0.0.1:18766` | Exposed via `get_api_base` |

```bash
export KALEIDO_DESKTOP_URL=https://kaleido.example.com/web/
export KALEIDO_API_BASE=https://kaleido.example.com
```

## Dev / build

Requires Linux WebKit + Tauri 2 system deps.

```bash
cd desktop-tauri/src-tauri
cargo run
cargo build --release
```

Or:

```bash
./desktop-tauri/scripts/dev.sh
```

## Non-goals

- Spawning `kaleido-server` as a child (use systemd)
- Re-adding 80+ upstream Tauri business commands
- Android packaging (Capacitor track; deferred)

## Layout

```
desktop-tauri/
  README.md
  web-stub/
  scripts/dev.sh
  src-tauri/
```
