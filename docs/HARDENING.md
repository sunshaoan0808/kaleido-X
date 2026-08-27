# Kaleido Server Hardening Notes (S5 / t6)

Operational hardening for the headless kaleido-server. **Docs + backup only** — no process restarts, no release rebuild.

## Live process invariant

| Rule | Detail |
|------|--------|
| Keep **18766** up | Baseline `kaleido-server` on `http://127.0.0.1:18766` is the live S4/S5 host process. |
| Do **not** kill / restart | Wave tasks must not `kill` the listener, stop systemd units, or `cargo build --release` for this host. |
| Backup is online-safe | `scripts/backup_data.sh` only reads `$KALEIDO_DATA` and writes a tarball; it never signals the server. |

Health check (read-only):

```bash
curl -sS http://127.0.0.1:18766/health
```

---

## 1. TLS / Caddy template

Public TLS is **not** implemented on the current host. Template only:

| Piece | Path / command |
|-------|----------------|
| Caddy reverse proxy | `docker/Caddyfile` |
| Compose profile | `docker compose --profile tls up -d` |
| Internal server bind | `server:18766` (compose network) |
| Host-only (no TLS) | `127.0.0.1:18766` published; hit HTTP directly |

### Caddyfile (template)

```caddy
# Replace kaleido.example.com with your domain; enable compose profile `tls`.
# docker compose --profile tls up -d

kaleido.example.com {
	encode gzip
	reverse_proxy server:18766

	header {
		Strict-Transport-Security "max-age=31536000; includeSubDomains"
		X-Content-Type-Options nosniff
		Referrer-Policy no-referrer
		-Server
	}
}
```

### Public exposure checklist (when enabling TLS later)

1. Set real hostname in `docker/Caddyfile` (not `kaleido.example.com`).
2. Ensure DNS A/AAAA points at the host; open only **80/443** publicly.
3. Keep app port **unpublished** or bound to loopback when Caddy fronts it:
   - Preferred: remove `ports:` on `server` and reach only via Caddy network.
   - Current compose publishes `127.0.0.1:18766:18766` for local/dev — safe for loopback-only.
4. Do **not** bind `0.0.0.0:18766` to the public internet without TLS + auth.
5. HSTS / security headers are already in the Caddyfile template.

> Out of scope for t6: obtaining real certificates or changing this host’s live bind.

---

## 2. Backup / restore

### Data layout (`$KALEIDO_DATA`)

Default host path is often `./data` or `/data` in containers (`KALEIDO_DATA`).

| Subdir | Contents | Backup? |
|--------|----------|---------|
| `state/` | users, markers | **yes** |
| `sessions/` | session tokens / records | **yes** |
| `jobs/` | job registry JSON | **yes** |
| `works/` | workspace files | **yes** |
| `artifacts/` | generated artifacts | **yes** |
| `audit/` | audit trails | **yes** |
| `secrets/` | server-held secrets (e.g. `llm_api_key.txt`) | **yes** (treat as sensitive) |
| `web/` | optional static copy | optional |
| `Kaleido/` | app data tree | **yes** |

### Backup script

```bash
# Dry-run (no write)
DRY_RUN=1 ./scripts/backup_data.sh

# Real backup → /tmp/kaleido-data-YYYYMMDD-HHMMSS.tar.gz
./scripts/backup_data.sh

# Custom source / destination directory
KALEIDO_DATA=/path/to/data OUT=/var/backups/kaleido ./scripts/backup_data.sh
```

Behavior:

- Reads `$KALEIDO_DATA` (default: `./data` relative to repo, or env).
- Writes timestamped tarball under `$OUT` (default `/tmp`).
- **Does not** stop, restart, or signal process **18766**.
- Supports `DRY_RUN=1` to print the planned archive path and file list without creating the tarball.
- Uses `tar` with relative paths; excludes obvious temp noise if present.

### Restore (manual)

```bash
# 1) Stop *only* when you intentionally take a maintenance window
#    (not during wave; live 18766 must stay up for S5 work).
# 2) Extract into an empty/target data dir
mkdir -p /path/to/restore-data
tar -xzf /tmp/kaleido-data-YYYYMMDD-HHMMSS.tar.gz -C /path/to/restore-data
# 3) Point KALEIDO_DATA at the restored tree and start a *new* instance
#    (do not overwrite live data while 18766 is writing without a plan).
```

Online backup consistency: JSON files may be mid-write. For critical restore points, prefer a quiet period or filesystem snapshot if available. The script never freezes the server.

---

## 3. Secrets handling

| Secret | Where | Rules |
|--------|-------|-------|
| Admin bootstrap | `.env` → `KALEIDO_ADMIN_USER` / `KALEIDO_ADMIN_PASSWORD` | Only used when `state/users.json` is empty; change default `change-me-strong-password`. |
| Session / auth | `$KALEIDO_DATA/sessions/`, hashed passwords under `state/` | Do not commit; back up with restricted perms (`600` / dir `700`). |
| LLM API key | `.env` `LLM_API_KEY` and/or `$KALEIDO_DATA/secrets/llm_api_key.txt` | Server injects credentials; client must not receive plaintext keys in API responses. |
| Compose / host env | `.env` (gitignored) | Copy from `.env.example`; never commit real `.env`. |

### Practices

1. **Never** commit `.env`, `data/secrets/`, or session dumps.
2. Prefer env / secret file over shipping keys in partner/settings client state.
3. Rotate `LLM_API_KEY` and admin password after any leak; invalidate sessions if tokens may be exposed.
4. Backup tarballs contain secrets — store under restricted directories and encrypt at rest when leaving the host (`gpg -c` optional).
5. Strip client-supplied empty key overwrites so server-held secrets are not clobbered (existing PartnerStore/settings behavior).

### What not to expose

| Surface | Public? | Notes |
|---------|---------|-------|
| `/health` | yes (info only) | phase/ok — no secrets |
| `/api/v1/public/info` | yes | capability flags only |
| `/api/v1/auth/login` | yes, rate-limited | no stack traces / user existence oracle beyond normal auth |
| Bearer-protected APIs | no without token | jobs, chat, mobile, settings |
| `$KALEIDO_DATA/**` | never via HTTP | filesystem only |
| `LLM_API_KEY` / `secrets/*` | never in JSON responses | inject server-side only |
| `RUST_LOG=debug` in prod | avoid | may log sensitive paths |
| Docker daemon / cargo target | not an API surface | keep host firewall tight |
| Port **18766** on `0.0.0.0` | avoid without TLS | prefer loopback + Caddy |

---

## 4. Rate limits & capacity

Configured via environment (see `.env.example`):

| Variable | Default (example) | Purpose |
|----------|-------------------|---------|
| `KALEIDO_LOGIN_MAX_ATTEMPTS` | `10` | Max login attempts per window (IP + username keys) |
| `KALEIDO_LOGIN_WINDOW_SECS` | `300` | Sliding window for login rate limit |
| `KALEIDO_SESSION_TTL_HOURS` | `12` | Session lifetime |
| `KALEIDO_MAX_SESSIONS` | `50` | Cap on active sessions |
| `KALEIDO_MAX_CONCURRENT_JOBS` | `2` (clamped 1–2) | Concurrent running jobs |

### Runtime behavior

| Path | At capacity |
|------|-------------|
| Login (`Auth::login`) | `CoreError::RateLimited` → HTTP **429** |
| Chat / `JobStore::try_start` | **429** RateLimited (fail fast, no queue) |
| `POST /api/v1/jobs` (`create`) | **queue** (`status=queued`); promote when a slot frees |

Audit recommendations:

1. Keep login limits enabled in any network-reachable deploy.
2. Do not raise `KALEIDO_MAX_CONCURRENT_JOBS` above host LLM/provider capacity.
3. Monitor 429s on login vs chat separately (abuse vs load).
4. Reverse proxy (Caddy) may add additional rate limiting later; app-level limits remain the baseline.

---

## 5. Host / compose quick reference

```bash
# Local (no TLS) — current wave style
export $(grep -v '^#' .env | xargs)
# live process already on 127.0.0.1:18766 — do not restart for t6

# Optional Docker (separate from live host process)
docker compose up -d --build          # server only
docker compose --profile tls up -d    # + Caddy when domain ready

# Backup (safe while live)
DRY_RUN=1 ./scripts/backup_data.sh
./scripts/backup_data.sh
```

---

## 6. t6 acceptance map

| Requirement | Covered |
|-------------|---------|
| TLS / Caddy template | §1 + `docker/Caddyfile` |
| Backup / restore | §2 + `scripts/backup_data.sh` |
| Secrets handling | §3 |
| Rate limits | §4 |
| What not to expose | §3 table |
| Live 18766 stays up | invariant + script does not stop server |
| Dry-run safe backup | `DRY_RUN=1` |

**Out of scope:** real public TLS on this host; Android; any `*.rs` / web / JobStore changes; killing 18766; `cargo build --release`.
