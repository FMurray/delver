#!/bin/sh
# Dev launcher for the Delver viewer (crates/viewer) against the local
# Postgres store. See docs/DECISIONS-viewer.md (DV-001..DV-010).
#
#   DATABASE_URL      Postgres store (default: the SHARED dev database, DV-010 —
#                     the viewer branch is merged and its embedded migrator
#                     matches the shared schema (v3); the old dedicated
#                     delver_viewer database is legacy/disposable)
#   DELVER_DOC_CACHE  byte-cache for original PDFs (default ~/.delver/doc-cache)
#   PDFIUM_LIBRARY_PATH  where libpdfium.dylib lives at runtime
#   DELVER_EMBED_ENDPOINT  optional, forwarded to template execution (DV-006)
#
# Mirrors crates/viewer/start.sh: pdfium is resolved from target/debug (the
# viewer build.rs drops the dylib there; we pre-seed it from deps/ so no
# network is needed), then `cargo leptos serve` builds server + wasm and
# serves on the address in [package.metadata.leptos] (127.0.0.1:3017).
set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

export DATABASE_URL="${DATABASE_URL:-postgres://delver:delver@localhost:5433/delver}"
export DELVER_DOC_CACHE="${DELVER_DOC_CACHE:-$HOME/.delver/doc-cache}"
mkdir -p "$DELVER_DOC_CACHE"

# pdfium runtime resolution (DV-005): build.rs early-returns when the dylib is
# already in target/debug; seed it from the deps/ checkout to avoid the
# GitHub download.
mkdir -p "$ROOT/target/debug"
if [ ! -f "$ROOT/target/debug/libpdfium.dylib" ] && [ -f "$ROOT/deps/pdfium-mac-arm64/lib/libpdfium.dylib" ]; then
    cp "$ROOT/deps/pdfium-mac-arm64/lib/libpdfium.dylib" "$ROOT/target/debug/"
fi
export PDFIUM_LIBRARY_PATH="$ROOT/target/debug"

cd "$ROOT"
exec cargo leptos serve
