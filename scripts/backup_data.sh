#!/usr/bin/env bash
# Online-safe backup of $KALEIDO_DATA for kaleido-server.
# Does NOT stop, restart, or signal the live process on :18766.
#
# Usage:
#   DRY_RUN=1 ./scripts/backup_data.sh
#   ./scripts/backup_data.sh
#   KALEIDO_DATA=/path/to/data OUT=/var/backups/kaleido ./scripts/backup_data.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SERVER_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

KALEIDO_DATA="${KALEIDO_DATA:-${SERVER_ROOT}/data}"
OUT="${OUT:-/tmp}"
DRY_RUN="${DRY_RUN:-0}"
STAMP="$(date -u +%Y%m%d-%H%M%S)"
ARCHIVE_NAME="kaleido-data-${STAMP}.tar.gz"
ARCHIVE_PATH="${OUT%/}/${ARCHIVE_NAME}"

log() { printf '[backup_data] %s\n' "$*"; }
die() { printf '[backup_data] ERROR: %s\n' "$*" >&2; exit 1; }

# Refuse anything that looks like "stop the server"
if [[ "${1:-}" == "--stop-server" ]] || [[ "${STOP_SERVER:-0}" == "1" ]]; then
  die "refusing to stop server; live 18766 must stay up (no STOP_SERVER / --stop-server)"
fi

[[ -d "${KALEIDO_DATA}" ]] || die "KALEIDO_DATA is not a directory: ${KALEIDO_DATA}"
[[ -d "${OUT}" ]] || die "OUT is not a directory: ${OUT}"

# Resolve to absolute for clearer logs
KALEIDO_DATA="$(cd "${KALEIDO_DATA}" && pwd)"
OUT="$(cd "${OUT}" && pwd)"
ARCHIVE_PATH="${OUT}/${ARCHIVE_NAME}"

log "source:  ${KALEIDO_DATA}"
log "output:  ${ARCHIVE_PATH}"
log "dry_run: ${DRY_RUN}"
log "note:    does not stop or signal kaleido-server (port 18766)"

# Parent of data dir + basename so tarball has a single top-level dir
PARENT="$(dirname "${KALEIDO_DATA}")"
BASE="$(basename "${KALEIDO_DATA}")"

# Optional excludes (temp / lock noise); keep secrets/ and jobs/ included
EXCLUDES=(
  --exclude="${BASE}/.tmp"
  --exclude="${BASE}/**/*.tmp"
  --exclude="${BASE}/**/*~"
)

if [[ "${DRY_RUN}" == "1" ]] || [[ "${DRY_RUN}" == "true" ]] || [[ "${DRY_RUN}" == "yes" ]]; then
  log "DRY_RUN: would create ${ARCHIVE_PATH}"
  log "DRY_RUN: inventory (paths under ${BASE}/):"
  # shellcheck disable=SC2086
  if tar -C "${PARENT}" "${EXCLUDES[@]}" -cf - "${BASE}" 2>/dev/null | tar -t 2>/dev/null | head -n 200; then
    :
  else
    # Fallback listing if tar stream fails in restricted env
    (cd "${PARENT}" && find "${BASE}" -type f | head -n 200)
  fi
  count="$(find "${KALEIDO_DATA}" -type f 2>/dev/null | wc -l | tr -d ' ')"
  log "DRY_RUN: approx file count=${count} (no archive written)"
  log "DRY_RUN: OK"
  exit 0
fi

# Real backup — read-only against live data; no flock on server
tar -C "${PARENT}" \
  "${EXCLUDES[@]}" \
  -czf "${ARCHIVE_PATH}" \
  "${BASE}"

# Restrictive perms: tarball may contain secrets/
chmod 600 "${ARCHIVE_PATH}" 2>/dev/null || true

size="$(wc -c < "${ARCHIVE_PATH}" | tr -d ' ')"
log "created ${ARCHIVE_PATH} (${size} bytes)"
log "done (server left running)"
