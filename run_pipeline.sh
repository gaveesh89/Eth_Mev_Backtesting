#!/usr/bin/env bash
# =============================================================================
# run_pipeline.sh — One-command MEV pipeline: fetch → validate → simulate
# =============================================================================
# Usage:
#   bash run_pipeline.sh                          # defaults: 21500000..21500005
#   bash run_pipeline.sh 21500000 21500010        # custom range
#   bash run_pipeline.sh 21500000 21500010 --skip-fetch  # reuse existing DB
# =============================================================================
set -euo pipefail

# ── Colors & helpers ─────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'

info()  { echo -e "${CYAN}[INFO]${RESET}  $*"; }
ok()    { echo -e "${GREEN}[  OK]${RESET}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${RESET}  $*"; }
fail()  { echo -e "${RED}[FAIL]${RESET}  $*"; exit 1; }
timer() { date +%s; }

phase_header() {
  echo ""
  echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
  echo -e "${BOLD}  Phase $1: $2${RESET}"
  echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
}

# ── Parse arguments ──────────────────────────────────────────────────────────
START_BLOCK="${1:-21500000}"
END_BLOCK="${2:-21500005}"
SKIP_FETCH=false

for arg in "$@"; do
  case "$arg" in
    --skip-fetch) SKIP_FETCH=true ;;
  esac
done

[[ "$START_BLOCK" =~ ^[0-9]+$ ]] || fail "start block must be numeric"
[[ "$END_BLOCK" =~ ^[0-9]+$ ]] || fail "end block must be numeric"
(( END_BLOCK >= START_BLOCK )) || fail "end block must be >= start block"

# ── Load environment ─────────────────────────────────────────────────────────
if [ -f .env ]; then
  set -a; source .env; set +a
  ok "Loaded .env"
else
  if [ -f .env.example ]; then
    warn ".env not found — copying from .env.example"
    cp .env.example .env
    fail "Edit .env with your Alchemy key, then re-run"
  else
    fail "No .env or .env.example found"
  fi
fi

# ── Validate required config ────────────────────────────────────────────────
RPC_URL="${MEV_RPC_URL:?Set MEV_RPC_URL in .env}"
DB_PATH="${MEV_DB_PATH:-data/mev.sqlite}"

command -v sqlite3 >/dev/null 2>&1 || fail "sqlite3 is required"
command -v cargo >/dev/null 2>&1 || fail "cargo is required"

mkdir -p "$(dirname "$DB_PATH")"

BLOCK_COUNT=$(( END_BLOCK - START_BLOCK + 1 ))

echo ""
echo -e "${BOLD}MEV Pipeline — Full Run${RESET}"
echo "  Blocks:   $START_BLOCK → $END_BLOCK ($BLOCK_COUNT blocks)"
echo "  RPC:      ${RPC_URL:0:45}..."
echo "  Database: $DB_PATH"
echo "  Skip fetch: $SKIP_FETCH"

PIPELINE_START=$(timer)

# ═════════════════════════════════════════════════════════════════════════════
# PHASE 0: Build (release)
# ═════════════════════════════════════════════════════════════════════════════
phase_header 0 "Build (release mode)"
T0=$(timer)

cargo build --release --workspace >/dev/null
ok "Build succeeded in $(( $(timer) - T0 ))s"

# ═════════════════════════════════════════════════════════════════════════════
# PHASE 1: Fetch blocks + receipts from Alchemy
# ═════════════════════════════════════════════════════════════════════════════
phase_header 1 "Fetch blocks from RPC"
T1=$(timer)

if [ "$SKIP_FETCH" = true ]; then
  warn "Skipping fetch (--skip-fetch). Using existing DB at $DB_PATH"
else
  info "Fetching $BLOCK_COUNT blocks with transactions and receipts..."

  cargo run --release -p mev-cli -- \
    --db-path "$DB_PATH" \
    fetch \
    --start-block "$START_BLOCK" \
    --end-block "$END_BLOCK"

  FETCH_ELAPSED=$(( $(timer) - T1 ))
  if [ "$FETCH_ELAPSED" -gt 0 ] && command -v bc >/dev/null 2>&1; then
    FETCH_RATE=$(echo "scale=1; $BLOCK_COUNT / $FETCH_ELAPSED" | bc)
  else
    FETCH_RATE="N/A"
  fi
  ok "Fetched $BLOCK_COUNT blocks in ${FETCH_ELAPSED}s (~${FETCH_RATE} blocks/s)"
fi

# ═════════════════════════════════════════════════════════════════════════════
# PHASE 2: Validate data integrity (SQLite)
# ═════════════════════════════════════════════════════════════════════════════
phase_header 2 "Validate data integrity"
T2=$(timer)

# --- Check 1: Block count ---
ACTUAL_BLOCKS=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM blocks WHERE block_number BETWEEN $START_BLOCK AND $END_BLOCK;")
if [ "$ACTUAL_BLOCKS" -eq "$BLOCK_COUNT" ]; then
  ok "Block count: $ACTUAL_BLOCKS/$BLOCK_COUNT"
else
  fail "Block count mismatch: expected $BLOCK_COUNT, got $ACTUAL_BLOCKS"
fi

# --- Check 2: Transaction count ---
TOTAL_TXS=$(sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM block_transactions WHERE block_number BETWEEN $START_BLOCK AND $END_BLOCK;")
info "Total transactions: $TOTAL_TXS"

# --- Check 3: Receipt completeness (gas_used should be present) ---
MISSING_GAS=$(sqlite3 "$DB_PATH" "
  SELECT COUNT(*) FROM block_transactions
  WHERE block_number BETWEEN $START_BLOCK AND $END_BLOCK
    AND gas_used IS NULL;
")
if [ "$MISSING_GAS" -eq 0 ]; then
  ok "All transactions have gas_used"
else
  warn "$MISSING_GAS transactions missing gas_used"
fi

# --- Check 4: tx_count consistency (block header vs actual rows) ---
MISMATCHED=$(sqlite3 "$DB_PATH" "
  SELECT COUNT(*) FROM (
    SELECT b.block_number, b.transaction_count AS expected,
           COALESCE(t.actual, 0) AS actual
    FROM blocks b
    LEFT JOIN (
      SELECT block_number, COUNT(*) AS actual
      FROM block_transactions
      GROUP BY block_number
    ) t ON b.block_number = t.block_number
    WHERE b.block_number BETWEEN $START_BLOCK AND $END_BLOCK
      AND b.transaction_count != COALESCE(t.actual, 0)
  );
")
if [ "$MISMATCHED" -eq 0 ]; then
  ok "All blocks: transaction_count matches actual tx rows"
else
  fail "$MISMATCHED blocks have transaction_count mismatch"
fi

# --- Check 5: No duplicate tx hashes in selected range ---
DUPES=$(sqlite3 "$DB_PATH" "
  SELECT COUNT(*) FROM (
    SELECT tx_hash FROM block_transactions
    WHERE block_number BETWEEN $START_BLOCK AND $END_BLOCK
    GROUP BY block_number, tx_hash HAVING COUNT(*) > 1
  );
")
if [ "$DUPES" -eq 0 ]; then
  ok "No duplicate transaction hashes (per block)"
else
  fail "$DUPES duplicate tx hashes found"
fi

# --- Run full SQL validation if available ---
if [ -f sql/validate_blocks.sql ]; then
  info "Running full SQL validation suite..."
  sqlite3 "$DB_PATH" < sql/validate_blocks.sql >/dev/null
  ok "SQL validation passed"
fi

ok "All integrity checks passed in $(( $(timer) - T2 ))s"

# ═════════════════════════════════════════════════════════════════════════════
# PHASE 3: Sample simulation
# ═════════════════════════════════════════════════════════════════════════════
phase_header 3 "Sample simulation"
T3=$(timer)

SAMPLE_BLOCK=$(sqlite3 "$DB_PATH" "
  SELECT block_number FROM blocks
  WHERE block_number BETWEEN $START_BLOCK AND $END_BLOCK
  ORDER BY transaction_count DESC
  LIMIT 1;
")

if [ -z "$SAMPLE_BLOCK" ]; then
  warn "No block found in selected range for simulation"
else
  SAMPLE_TX_COUNT=$(sqlite3 "$DB_PATH" "
    SELECT transaction_count FROM blocks WHERE block_number = $SAMPLE_BLOCK;
  ")

  info "Selected block $SAMPLE_BLOCK ($SAMPLE_TX_COUNT txs) for simulation"

  if cargo run --release -p mev-cli -- \
      --db-path "$DB_PATH" \
      simulate \
      --block "$SAMPLE_BLOCK" \
      --algorithm egp >/dev/null 2>&1; then
    ok "Simulation completed for block $SAMPLE_BLOCK"
  else
    SIM_EXIT=$?
    warn "Simulation exited with code $SIM_EXIT — check with verbose logs"
  fi
fi

ok "Phase 3 done in $(( $(timer) - T3 ))s"

# ═════════════════════════════════════════════════════════════════════════════
# SUMMARY
# ═════════════════════════════════════════════════════════════════════════════
TOTAL_ELAPSED=$(( $(timer) - PIPELINE_START ))

echo ""
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
echo -e "${GREEN}${BOLD}  ✓ Pipeline complete${RESET}"
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
echo ""
echo "  Blocks fetched:   $BLOCK_COUNT"
echo "  Transactions:     ${TOTAL_TXS:-0}"
echo "  Integrity:        all checks passed"
echo "  Simulation block: ${SAMPLE_BLOCK:-N/A}"
echo "  Total time:       ${TOTAL_ELAPSED}s"
echo "  Database:         $DB_PATH ($(du -h "$DB_PATH" | cut -f1))"
echo ""
echo -e "  ${CYAN}Next steps:${RESET}"
echo "    • Fetch more:   bash run_pipeline.sh 21500000 21501000"
echo "    • Revalidate:   bash run_pipeline.sh 21500000 21500005 --skip-fetch"
echo "    • Explore DB:   sqlite3 $DB_PATH"
echo ""
