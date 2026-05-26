#!/usr/bin/env bash
# benchmark-exec.sh — compare rattler exec vs pixi exec
# Requires: hyperfine (cargo install hyperfine)
#
# All cache data is written to ./cache/exec/benchmark/ — nothing touches
# system cache to keep the flow clear.

set -euo pipefail

# ── Paths ────────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

RATTLER_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PIXI_ROOT="/Users/dagmardinjens/Documents/prefix/pixi"

RATTLER_BIN="$RATTLER_ROOT/target/release/rattler"
PIXI_BIN="$PIXI_ROOT/target/release/pixi"

BENCH_ROOT="$SCRIPT_DIR/benchmark"
EXPORT_DIR="$SCRIPT_DIR/results"

# ── Preflight checks ─────────────────────────────────────────────────────────
echo "Building rattler (release)..."
cargo build --release --manifest-path "$RATTLER_ROOT/Cargo.toml"

echo "Building pixi (release)..."
cargo build --release --manifest-path "$PIXI_ROOT/Cargo.toml"

if [[ ! -x "$RATTLER_BIN" ]]; then
  echo "ERROR: $RATTLER_BIN not found or not executable after build." >&2
  exit 1
fi

if [[ ! -x "$PIXI_BIN" ]]; then
  echo "ERROR: $PIXI_BIN not found or not executable after build." >&2
  exit 1
fi

if ! command -v hyperfine &>/dev/null; then
  echo "ERROR: hyperfine not found. Install with: cargo install hyperfine" >&2
  exit 1
fi

# ── Tool invocations ─────────────────────────────────────────────────────────
# RATTLER_CACHE_DIR controls both tools: pixi falls back to it if PIXI_CACHE_DIR is unset.
CACHE_ENV="env RATTLER_CACHE_DIR=$BENCH_ROOT"
RATTLER="$RATTLER_BIN exec"
PIXI="$PIXI_BIN exec"

# ── Bootstrap dirs ───────────────────────────────────────────────────────────
echo "Creating local cache dirs under $BENCH_ROOT"
mkdir -p "$BENCH_ROOT"
mkdir -p "$EXPORT_DIR"

# ── Purge helpers ────────────────────────────────────────────────────────────
purge_exec_envs() {
  rm -rf "$BENCH_ROOT/cached-envs-v0"
  rm -rf "$BENCH_ROOT/repodata"
}

purge_all() {
  rm -rf "$BENCH_ROOT"
  mkdir -p "$BENCH_ROOT" "$EXPORT_DIR"
}

# ── Shared hyperfine helpers ─────────────────────────────────────────────────
# NOTE: cold runs wipe both the solved-env dirs AND the package cache so that
# every trial does a true solve+download+install. This makes cold runs slow
# (~minutes per trial) but means the numbers are actually comparable.
# Warm runs leave the package cache intact — that is intentional.
hf_cold() {
  local label="$1"; shift
  hyperfine \
    --runs 5 \
    --prepare "rm -rf '$BENCH_ROOT/benchmark'" \
    --export-markdown "$EXPORT_DIR/${label}.md" \
    --export-json     "$EXPORT_DIR/${label}.json" \
    "$@"
}

hf_warm() {
  local label="$1"; shift
  hyperfine \
    --warmup 2 \
    --runs 10 \
    --export-markdown "$EXPORT_DIR/${label}.md" \
    --export-json     "$EXPORT_DIR/${label}.json" \
    "$@"
}

# ════════════════════════════════════════════════════════════════════════════
echo
echo "══════════════════════════════════════════════════"
echo " 1. COLD — guessed package  (full solve+install)"
echo "══════════════════════════════════════════════════"
purge_all
hf_cold "01-cold-guessed" \
  --command-name "rattler (cold, guessed)" "$CACHE_ENV $RATTLER python -c 'exit(0)'" \
  --command-name "pixi   (cold, guessed)" "$CACHE_ENV $PIXI   python -c 'exit(0)'"

# ════════════════════════════════════════════════════════════════════════════
echo
echo "══════════════════════════════════════════════════"
echo " 2. WARM — guessed package  (cache hit overhead)"
echo "══════════════════════════════════════════════════"
$CACHE_ENV $RATTLER python -c 'exit(0)' 2>/dev/null || true
$CACHE_ENV $PIXI    python -c 'exit(0)' 2>/dev/null || true

hf_warm "02-warm-guessed" \
  --command-name "rattler (warm, guessed)" "$CACHE_ENV $RATTLER python -c 'exit(0)'" \
  --command-name "pixi   (warm, guessed)" "$CACHE_ENV $PIXI   python -c 'exit(0)'"

# ════════════════════════════════════════════════════════════════════════════
echo
echo "══════════════════════════════════════════════════"
echo " 3. COLD — explicit --spec   (pinned version)"
echo "══════════════════════════════════════════════════"
hf_cold "03-cold-spec" \
  --command-name "rattler (cold, --spec)" "$CACHE_ENV $RATTLER --spec python=3.12 python -c 'exit(0)'" \
  --command-name "pixi   (cold, --spec)" "$CACHE_ENV $PIXI   --spec python=3.12 python -c 'exit(0)'"

# ════════════════════════════════════════════════════════════════════════════
echo
echo "══════════════════════════════════════════════════"
echo " 4. COLD — --with            (exercises retry path)"
echo "══════════════════════════════════════════════════"
hf_cold "04-cold-with" \
  --command-name "rattler (cold, --with)" "$CACHE_ENV $RATTLER --with numpy python -c 'exit(0)'" \
  --command-name "pixi   (cold, --with)" "$CACHE_ENV $PIXI   --with numpy python -c 'exit(0)'"

# ════════════════════════════════════════════════════════════════════════════
echo
echo "══════════════════════════════════════════════════"
echo " 5. WARM — --with            (multi-spec hash lookup)"
echo "══════════════════════════════════════════════════"
$CACHE_ENV $RATTLER --with numpy python -c 'exit(0)' 2>/dev/null || true
$CACHE_ENV $PIXI    --with numpy python -c 'exit(0)' 2>/dev/null || true

hf_warm "05-warm-with" \
  --command-name "rattler (warm, --with)" "$CACHE_ENV $RATTLER --with numpy python -c 'exit(0)'" \
  --command-name "pixi   (warm, --with)" "$CACHE_ENV $PIXI   --with numpy python -c 'exit(0)'"

# ════════════════════════════════════════════════════════════════════════════
echo
echo "Results written to $EXPORT_DIR"
echo
purge_all
ls -1 "$EXPORT_DIR"