#!/usr/bin/env bash
# Local OTLP collector + trace UI for `delver --trace` (docs/TRACING.md, D-027).
#
# Downloads the single-binary Jaeger v2 from GitHub releases (no Docker —
# Docker Desktop is org-gated) into ~/.delver/bin, verifies its sha256, and
# starts it with the v2 all-in-one defaults: OTLP/HTTP ingest on :4318,
# OTLP/gRPC on :4317, UI on :16686, in-memory storage (traces vanish on
# restart — this is a dev viewer, not storage).
#
# Idempotent: reuses a downloaded binary, exits early when a collector is
# already answering on the UI port. Stop it with:  kill $(cat "$PID_FILE")
set -euo pipefail

JAEGER_VERSION="${JAEGER_VERSION:-2.19.0}"
BIN_DIR="${DELVER_BIN_DIR:-$HOME/.delver/bin}"
UI_URL="http://localhost:16686"
OTLP_URL="http://localhost:4318"
LOG_FILE="${TMPDIR:-/tmp}/delver-jaeger.log"
PID_FILE="${TMPDIR:-/tmp}/delver-jaeger.pid"

# ── already running? ─────────────────────────────────────────────────────────
if curl -fsS -o /dev/null --max-time 2 "$UI_URL" 2>/dev/null; then
    echo "Collector already running — UI: $UI_URL   OTLP: $OTLP_URL"
    exit 0
fi

# ── resolve platform + fetch (once) ──────────────────────────────────────────
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$arch" in
    x86_64) arch="amd64" ;;
    aarch64 | arm64) arch="arm64" ;;
esac
name="jaeger-${JAEGER_VERSION}-${os}-${arch}"
bin="$BIN_DIR/jaeger-${JAEGER_VERSION}"

if [ ! -x "$bin" ]; then
    mkdir -p "$BIN_DIR"
    base="https://github.com/jaegertracing/jaeger/releases/download/v${JAEGER_VERSION}"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    echo "Downloading ${base}/${name}.tar.gz"
    curl -fSL --retry 3 -o "$tmp/${name}.tar.gz" "${base}/${name}.tar.gz"
    curl -fsSL --retry 3 -o "$tmp/${name}.sha256sum.txt" "${base}/${name}.sha256sum.txt"
    # The release .sha256sum.txt lists the EXTRACTED file paths, not the
    # tarball — extract first, then verify from the same directory.
    tar -xzf "$tmp/${name}.tar.gz" -C "$tmp"
    (cd "$tmp" && shasum -a 256 -c "${name}.sha256sum.txt" >/dev/null) \
        || { echo "sha256 verification FAILED for ${name} contents" >&2; exit 1; }
    mv "$tmp/${name}/jaeger" "$bin"
    chmod +x "$bin"
    echo "Installed $bin"
fi

# ── start + wait for the UI ──────────────────────────────────────────────────
nohup "$bin" >"$LOG_FILE" 2>&1 &
pid=$!
echo "$pid" >"$PID_FILE"
for _ in $(seq 1 60); do
    if curl -fsS -o /dev/null --max-time 1 "$UI_URL" 2>/dev/null; then
        echo "Jaeger v${JAEGER_VERSION} running (pid $pid; pid file $PID_FILE)"
        echo "  UI:   $UI_URL"
        echo "  OTLP: $OTLP_URL   (delver <cmd> --trace exports here)"
        echo "  log:  $LOG_FILE"
        exit 0
    fi
    sleep 0.5
done
echo "Jaeger did not come up within 30s; log tail:" >&2
tail -20 "$LOG_FILE" >&2
exit 1
