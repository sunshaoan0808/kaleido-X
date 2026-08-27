#!/usr/bin/env bash
set -euo pipefail

# Kaleido Server + Embed Proxy — install / status / restart helper
# Usage:
#   ./kaleido-deploy.sh install   — install + enable + start both services
#   ./kaleido-deploy.sh status    — status of both services
#   ./kaleido-deploy.sh restart   — restart both services
#   ./kaleido-deploy.sh logs      — tail logs of both services
#   ./kaleido-deploy.sh uninstall — stop + disable + remove services

REPO="$(cd "$(dirname "$0")/.." && pwd)"
SVC_DIR="${REPO}/scripts"

install_services() {
    echo "→ Installing kaleido-embed-proxy.service..."
    cp "${SVC_DIR}/kaleido-embed-proxy.service" /etc/systemd/system/
    echo "→ Installing kaleido-server.service..."
    cp "${SVC_DIR}/kaleido-server.service" /etc/systemd/system/
    systemctl daemon-reload
    systemctl enable --now kaleido-embed-proxy
    systemctl enable --now kaleido-server
    echo "✓ Services installed and started"
}

check_status() {
    echo "=== Embed Proxy ==="
    systemctl status kaleido-embed-proxy --no-pager 2>&1 | head -10
    echo
    echo "=== Kaleido Server ==="
    systemctl status kaleido-server --no-pager 2>&1 | head -10
}

restart_services() {
    echo "→ Restarting services..."
    systemctl restart kaleido-embed-proxy
    systemctl restart kaleido-server
    echo "✓ Restarted"
    sleep 2
    curl -sf http://127.0.0.1:20145/health && echo " embed-proxy healthy" || echo " embed-proxy NOT healthy"
    curl -sf http://127.0.0.1:18766/health && echo " kaleido-server healthy" || echo " kaleido-server NOT healthy"
}

tail_logs() {
    journalctl -u kaleido-embed-proxy -u kaleido-server -f --no-pager
}

uninstall_services() {
    echo "→ Stopping and removing services..."
    systemctl stop kaleido-server 2>/dev/null || true
    systemctl stop kaleido-embed-proxy 2>/dev/null || true
    systemctl disable kaleido-server 2>/dev/null || true
    systemctl disable kaleido-embed-proxy 2>/dev/null || true
    rm -f /etc/systemd/system/kaleido-server.service
    rm -f /etc/systemd/system/kaleido-embed-proxy.service
    systemctl daemon-reload
    echo "✓ Services removed"
}

case "${1:-status}" in
    install)   install_services ;;
    status)    check_status ;;
    restart)   restart_services ;;
    logs)      tail_logs ;;
    uninstall) uninstall_services ;;
    *)
        echo "Usage: $0 {install|status|restart|logs|uninstall}"
        exit 1
        ;;
esac
