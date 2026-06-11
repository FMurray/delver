#!/usr/bin/env bash
# Fetch document corpora for LOCAL testing of delver.
#
# ┌─────────────────────────────────────────────────────────────────────────┐
# │ COMPLIANCE — READ BEFORE RUNNING                                        │
# │ This repository is PUBLIC. Datasets fetched by this script are for      │
# │ LOCAL testing only: never commit, redistribute, deploy, or upload them. │
# │ Data lands OUTSIDE the repo ($DELVER_TESTDATA, default ~/datasets).     │
# │ Customer-derived corpora (PMBench, CustomerBench, Ares) are PROHIBITED. │
# └─────────────────────────────────────────────────────────────────────────┘
set -euo pipefail

DEST="${DELVER_TESTDATA:-$HOME/datasets}"
mkdir -p "$DEST"
echo "==> test data destination: $DEST (outside repo)"

# ── 1. Single-doc fixture: 3M 2015 10-K (delver's historical demo doc) ──────
if [[ ! -f "$DEST/3M_2015_10K.pdf" ]]; then
  echo "==> fetching 3M 2015 10-K (1.2 MB)"
  curl -sL -o "$DEST/3M_2015_10K.pdf" \
    https://raw.githubusercontent.com/patronus-ai/financebench/main/pdfs/3M_2015_10K.pdf
else
  echo "==> 3M 10-K already present"
fi

# ── 2. FinanceBench: ~360 SEC filing PDFs + 150 QA pairs (~1.1 GB) ──────────
# License note: QA labels are CC-BY-NC (local dev only). Underlying SEC filings
# are public records (re-fetchable from EDGAR).
if [[ ! -d "$DEST/financebench" ]]; then
  echo "==> cloning FinanceBench (~565 MB download)"
  git clone --depth 1 https://github.com/patronus-ai/financebench.git "$DEST/financebench"
else
  echo "==> financebench already present"
fi

# ── 3. OfficeQA: Treasury Bulletin PDFs + QA (hard-133 = officeqa_pro.csv) ──
# GATED on Hugging Face: requires one-time `hf auth login` and accepting the
# dataset terms at https://huggingface.co/datasets/databricks/officeqa
if [[ "${FETCH_OFFICEQA:-0}" == "1" ]]; then
  if ! command -v hf >/dev/null 2>&1; then
    echo "!! hf CLI not found: pip install -U 'huggingface_hub[cli]'" >&2; exit 1
  fi
  echo "==> fetching OfficeQA QA CSVs + Treasury PDFs (~4 GB; gated — needs hf auth login)"
  hf download databricks/officeqa --repo-type dataset \
    --include "officeqa_pro.csv" "officeqa_full.csv" "treasury_bulletin_pdfs/*" \
    --local-dir "$DEST/officeqa"
  [[ -d "$DEST/officeqa-harness" ]] || \
    git clone --depth 1 https://github.com/databricks/officeqa.git "$DEST/officeqa-harness"
else
  echo "==> skipping OfficeQA (set FETCH_OFFICEQA=1 after accepting the HF gate + hf auth login)"
fi

# ── 4. OmniDocBench (optional): layout/table/reading-order ground truth ─────
if [[ "${FETCH_OMNIDOCBENCH:-0}" == "1" ]]; then
  hf download opendatalab/OmniDocBench --repo-type dataset --local-dir "$DEST/omnidocbench"
else
  echo "==> skipping OmniDocBench (set FETCH_OMNIDOCBENCH=1 to fetch)"
fi

echo "==> done. Point tests at it with: export DELVER_TESTDATA=$DEST"
