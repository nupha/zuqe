#!/usr/bin/env bash
# M2b-7: feature-gating matrix for the `jit` feature.
#
# D-19 kept `jit` in `default`, so the "off" leg spells the default feature
# list explicitly (default minus `jit`). Both legs must be green, and the off
# build must not pull cranelift into the dependency graph.
#
# The platform matrix is {off,on} x {linux x86_64, linux aarch64, macOS arm64};
# this script runs the legs available on the current host and prints the row it
# covered. Cross-rows need a runner with that host.
set -euo pipefail
cd "$(dirname "$0")/../.."

OFF_FEATURES="inline_cache,big_int,all_unicode,async_support,builtin_mapset,builtin_proxy,builtin_atomic,builtin_regexp,builtin_reflect,builtin_json,builtin_date,builtin_math,builtin_weakref"

echo "== host: $(uname -s)/$(uname -m)"

echo "== off: dependency graph"
if cargo tree -e features --no-default-features --features "$OFF_FEATURES" | grep -i cranelift; then
    echo "FAIL: cranelift is reachable with the jit feature off"
    exit 1
fi
echo "ok: no cranelift with the feature off"

run_tests() {
    local label=$1
    shift
    local out
    if ! out=$("$@" 2>&1); then
        echo "$out" | tail -40
        echo "FAIL: ${label}"
        exit 1
    fi
    if echo "$out" | grep -q "^test result: FAILED"; then
        echo "$out" | tail -40
        echo "FAIL: ${label} (reported FAILED)"
        exit 1
    fi
    echo "$out" | grep -E "^test result" | sed "s/^/${label}: /"
}

echo "== off: cargo test (jit removed from the default feature set)"
run_tests "off" cargo test --no-default-features --features "$OFF_FEATURES"

echo "== on: cargo test (default features, jit included)"
run_tests "on" cargo test

check_compiles() {
    local label=$1
    shift
    local out
    if ! out=$("$@" 2>&1); then
        echo "$out" | tail -40
        echo "FAIL: ${label}"
        exit 1
    fi
    echo "ok: ${label}"
}

# The {off,on} legs only build `default`. Optional non-default features change
# runtime structure, and a break under one of them once shipped through an
# all-green run: a `child_runtime`-only compile error in `tick.rs`, and a
# `quantum_poll` missing-import error in `park.rs` (both found by hand). Compile
# checks catch that class and stay cheap; they are not test runs.
echo "== extra: non-default feature combinations compile with the jit feature"
for extra in child_runtime persistent crossbeam_channel serde_json_support dump_bytecode quantum_poll; do
    check_compiles "on+${extra}" cargo check -q --features "jit,${extra}"
done

echo "== matrix row covered: $(uname -s)/$(uname -m) {off,on}"
echo "OK"
