#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>
#
# Regression check for the post-call OSR duplication blowup (2026-10-29,
# `OSR_TAIL_BUDGET`; timeline in `memo/jit_impl.md` §12.4). This script cites no
# other document: it used to point at `memo/bytecode-bugs.md` ("Bug 3"), which
# was merged away, so the cause is stated right here.
#
# test/repro.js is one straight-line basic block holding 500 calls. Before the
# OSR budget, every call earned its own post-call entry, each entry re-lowered
# its block's whole tail into a Cranelift block of its own, and the copies stayed
# in the compiled code -- O(calls x block length). The function blew past
# Cranelift's `CodeTooLarge`, took ~31 s per attempt, and lost native code.
#
# The check is the ratio, not the wall clock: compilation is a background worker
# with a 30 s budget per attempt, so a blowup shows up as a long run AND as a
# shortfall between verified and compiled functions. Both are asserted.

set -euo pipefail

cd "$(dirname "$0")/../../.."

BIN=./target/release/examples/test262
CFG=tests/jit/ir-blowup/test262.yml

# Generous next to the healthy ~1 s, far below the ~41 s blowup.
MAX_SECONDS=${MAX_SECONDS:-20}

cargo build --release --example test262 --features intl402

out=$("$BIN" --config "$CFG" --jit 2>&1 | tr '\r' '\n')
metrics=$(printf '%s\n' "$out" | grep -E '^JIT metrics' | tail -1)
done_line=$(printf '%s\n' "$out" | grep -E '^Done' | tail -1)

if [ -z "$metrics" ] || [ -z "$done_line" ]; then
    printf '%s\n' "$out" | tail -20
    echo "FAIL: the repro did not run (no Done / JIT metrics line)"
    exit 1
fi

echo "$done_line"
echo "$metrics"

# The repro must still pass: this check is about compile cost, not semantics.
case "$done_line" in
    *"0 failed") ;;
    *) echo "FAIL: the repro no longer passes"; exit 1 ;;
esac

field() { printf '%s\n' "$metrics" | sed -n "s/.*$1=\([0-9]*\).*/\1/p"; }
verified=$(field functions_verified)
compiled=$(field functions_compiled)
failures=$(field compile_failures)

# Every verified function must compile. The blowup loses exactly one: the
# call-heavy block that cannot fit in CodeTooLarge.
if [ "$verified" != "$compiled" ] || [ "$failures" != "0" ]; then
    echo "FAIL: verified=$verified compiled=$compiled compile_failures=$failures"
    echo "      post-call OSR entries are duplicating blocks again (see this script's header)"
    exit 1
fi

# Wall clock is the coarse signal; the compiler's own 30 s budget per attempt
# means an unnoticed blowup costs at least that much per retry.
start=$(date +%s)
"$BIN" --config "$CFG" --jit >/dev/null 2>&1
elapsed=$(( $(date +%s) - start ))
if [ "$elapsed" -gt "$MAX_SECONDS" ]; then
    echo "FAIL: took ${elapsed}s, over the ${MAX_SECONDS}s ceiling"
    exit 1
fi

echo "OK: compiled=$compiled/$verified, ${elapsed}s (ceiling ${MAX_SECONDS}s)"