#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Phase 0 GC baseline driver: run each kernel `RUNS` times in a fresh process
# and report per-kernel medians. One kernel per process matches
# tests/bench_gc_baseline.mjs; no A/B here (there is only the current binary).
#
#   tests/bench_gc_baseline.sh [N]
#
# Env: ZJS (host binary), RUNS (default 6).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ZJS="${ZJS:-$ROOT/target/release/examples/zjs}"
RUNS="${RUNS:-6}"
N="${1:-200000}"
KERNELS="churn-ctor churn-objlit retain-objects mixed"

if [[ ! -x "$ZJS" ]]; then
    echo "missing $ZJS (build with: cargo build --release --example zjs)" >&2
    exit 1
fi

OUT="$(mktemp)"
trap 'rm -f "$OUT"' EXIT

for kernel in $KERNELS; do
    for _ in $(seq 1 "$RUNS"); do
        "$ZJS" "$ROOT/tests/bench_gc_baseline.mjs" "$kernel" "$N" | tail -n 1 >>"$OUT"
    done
done

python3 - "$OUT" "$RUNS" <<'PY'
import json, re, statistics, sys

path, runs = sys.argv[1], int(sys.argv[2])
rows = {}
with open(path) as f:
    for line in f:
        # zjs colorizes the printed value; strip ANSI before parsing.
        line = re.sub(r"\x1b\[[0-9;]*m", "", line).strip()
        if not line:
            continue
        row = json.loads(line)
        rows.setdefault(row["kernel"], []).append(row)

cols = ["wallMs", "gcMs", "gcShare", "peakBytes", "majors", "minors"]
print(f"{'kernel':<16}{'runs':>5}" + "".join(f"{c:>12}" for c in cols))
for kernel, rs in rows.items():
    med = {c: statistics.median(r[c] for r in rs) for c in cols}
    print(f"{kernel:<16}{len(rs):>5}" + "".join(f"{med[c]:>12.4g}" for c in cols))
PY
