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
# Output: one line per grid point with the per-kernel medians (ms), plus the
# summary lines. `awk` at the end turns it into a per-kernel winner table.
set -eu
cd "$(dirname "$0")/../.."

BENCH=target/release/examples/jit-bench
cargo build --release --features jit --example jit-bench

CALLS="32 64 128 256 512"
LOOPS="56 128 256 1024"

for call in $CALLS; do
    for loop in $LOOPS; do
        echo "### call=$call loop=$loop"
        "$BENCH" --modes automatic \
            --call-threshold "$call" --loop-threshold "$loop" \
            | sed -n '/^kernel /,/^results:/p'
    done
done
