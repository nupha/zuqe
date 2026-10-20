#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>
#
# TH1: sweep the compile thresholds over the 35 bench kernels (plan
# §13.6.6). The grid is
#
#   call_threshold ∈ {32, 64, 128, 256, 512}
#   loop_threshold ∈ {56, 128, 256, 1024}
#
# and the criterion is the *per-kernel median* (the bench reports it), not the
# average: the two slow kernels would drag an average around.
#
# The tier-2 gate (TH2) is left at its default: this sweep is about the first
# gate. The `automatic` policy is the production one, so that is the column
# measured; the interpreter column is the baseline the bench always runs.
#
# P0-4 adds `--joint`: the **two gates together**. The first gate is swept
# over three points and the second (the tier-2 promotion) over three, so the
# grid is 3 × 3 = 9 points. The summary at the end takes, for every kernel,
# the ratio of a grid point's median to that kernel's best median, and prints
# the median of those ratios per point — the same criterion TH1 used.
#
# Output: one TSV line per (grid point, kernel) — `point <TAB> kernel <TAB>
# median ms` — plus the summary lines. `awk` at the end turns it into the
# per-point median ratio table.
set -eu
cd "$(dirname "$0")/../.."

BENCH=target/release/examples/jit-bench
cargo build --release --features jit --example jit-bench

CALLS="32 64 128 256 512"
LOOPS="56 128 256 1024"

# The joint grid: the first gate × the second gate. The gates are swept as
# *pairs* (call/loop), so the grid is 3 × 3 = 9 points, not 3⁴.
JOINT_GATES="32:56 128:128 512:1024"
TIER2_GATES="256:512 1024:2048 4096:8192"

ROWS=$(mktemp)
trap 'rm -f "$ROWS"' EXIT

# Run one grid point and record its per-kernel medians.
run_point() {
    label=$1
    shift
    echo "### $label"
    "$BENCH" --modes automatic "$@" \
        | sed -n '/^kernel /,/^results:/p' \
        | awk -v label="$label" 'NF > 2 && $2 ~ /ms$/ { gsub(/ms$/, "", $2); print label "\t" $1 "\t" $2 }' \
        | tee -a "$ROWS"
}

# The summary: per point, the median over the kernels of (this point / the
# best point for that kernel).
summarize() {
    awk -F'\t' '
    {
        n++;
        p[n] = $1; k[n] = $2; v[n] = $3 + 0;
        if (!($2 in best) || $3 + 0 < best[$2]) best[$2] = $3 + 0;
        if (!($1 in seen)) { seen[$1] = 1; order[++np] = $1 }
    }
    END {
        for (i = 1; i <= n; i++) r[i] = v[i] / best[k[i]];
        for (pi = 1; pi <= np; pi++) {
            m = 0;
            for (j = 1; j <= n; j++) if (p[j] == order[pi]) ratios[++m] = r[j];
            for (a = 2; a <= m; a++) {
                x = ratios[a]; b = a - 1;
                while (b > 0 && ratios[b] > x) { ratios[b + 1] = ratios[b]; b-- }
                ratios[b + 1] = x
            }
            printf "%-28s median=%.3f worst=%.3f\n", order[pi], ratios[int((m + 1) / 2)], ratios[m];
        }
    }' "$ROWS"
}

if [ "${1:-}" = "--joint" ]; then
    for gate in $JOINT_GATES; do
        call=${gate%%:*}
        loop=${gate##*:}
        for t2gate in $TIER2_GATES; do
            t2call=${t2gate%%:*}
            t2loop=${t2gate##*:}
            run_point "call=$call loop=$loop t2call=$t2call t2loop=$t2loop" \
                --call-threshold "$call" --loop-threshold "$loop" \
                --tier2-call-threshold "$t2call" --tier2-loop-threshold "$t2loop"
        done
    done
    echo "### summary (per-kernel median ratio against the best point of each kernel)"
    summarize
    exit 0
fi

for call in $CALLS; do
    for loop in $LOOPS; do
        run_point "call=$call loop=$loop" --call-threshold "$call" --loop-threshold "$loop"
    done
done
