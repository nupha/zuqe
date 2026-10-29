#!/usr/bin/env bash
# M2b-7: feature-gating matrix for the JIT feature.
#
# D-19 kept `jit` in `default`, so the "off" leg spells the default feature
# list explicitly (default minus `jit`). Both legs must be green, and the off
# build must not pull cranelift into the dependency graph.
#
# `jit` is in `default`, so the plain run exercises the worker model.
#
# The platform matrix is {off,worker} x {linux x86_64, linux aarch64,
# macOS x86_64, macOS arm64}; this script runs the legs available on the
# current host and prints the row it covered. Cross-rows need a runner with
# that host.
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

echo "== worker: cargo test (default features, jit)"
run_tests "worker" cargo test

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

# The off/worker legs only build the targets `cargo test` needs.
# Optional non-default features change runtime structure, and a break under one
# of them once shipped through an all-green run: a `child_runtime`-only compile
# error in `tick.rs`, and a `quantum_poll` missing-import error in `park.rs`
# (both found by hand). Compile checks catch that class and stay cheap; they are
# not test runs.
echo "== extra: non-default feature combinations compile with the jit feature"
for extra in child_runtime persistent crossbeam_channel serde_json_support dump_bytecode quantum_poll; do
    check_compiles "on+${extra}" cargo check -q --features "jit,${extra}"
done

# `jit_dump` (the IR/code dump) is a diagnostic, not a second baseline: one
# compile leg for the combined feature set, one smoke test (the dump module's
# own test), and the whole suite keeps running under the default features.
echo "== extra: jit_dump + dump_bytecode compile together"
check_compiles "on+jit,dump_bytecode,jit_dump" cargo check -q --features "jit,dump_bytecode,jit_dump"

echo "== jit_dump: smoke (dump one function, assert .clif/.bin exist)"
run_tests "jit_dump" cargo test -q --features jit_dump --lib jit::dump

# Benches/examples are not built by `cargo test` unless asked for; the compiler
# workers change the JIT's public behaviour, so compile them too.
echo "== extra: every target compiles under default features"
check_compiles "default (all targets)" cargo check -q --all-targets

# The dump must be *absent* from a build without the feature: a leaked
# `#[cfg]` would ship the diagnostics path (and the `JIT_DUMP` env read) in
# production. Built here with the default feature set (jit_dump is not in
# `default`), asserted with `strings`, which sees literals even in a stripped
# binary (the release profile strips symbols).
echo "== jit_dump: absent from the default release artifact"
cargo build --release --example jit-bench
DUMP_STRINGS=$(strings ./target/release/examples/jit-bench | grep -cE 'JIT_DUMP|zuqe jit_dump|\.clif' || true)
if [ "$DUMP_STRINGS" != "0" ]; then
    echo "FAIL: ${DUMP_STRINGS} dump string(s) in the release artifact (jit_dump gate leaked)"
    exit 1
fi
echo "ok: no dump strings in the release binary"

echo "== matrix row covered: $(uname -s)/$(uname -m) {off,worker}"
echo "OK"
