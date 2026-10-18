#!/usr/bin/env bash
# M2b-8: hardening matrix for the JIT.
#
# Legs:
#   1. dev build (debug_assertions on) — the default `cargo test` run
#   2. release build (panic = abort, no unwinding)
#   3. fault injection: the compile pipeline must survive a refused mmap
#   4. AddressSanitizer (nightly), gated on a probe that proves the engine can
#      create a context at all under the sanitizer
set -euo pipefail
cd "$(dirname "$0")/../.."

TARGET=x86_64-unknown-linux-gnu

echo "== dev (debug_assertions)"
cargo test --features jit --lib --tests | grep -E "^test result" | sed 's/^/dev: /'

echo "== release (panic=abort)"
cargo test --release --features jit --lib --tests | grep -E "^test result" | sed 's/^/release: /'

echo "== fault injection (refused mmap -> settle in the interpreter)"
cargo test --features jit --lib allocation_failure | grep -E "^test result" | sed 's/^/fault: /'

if ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
    echo "== SKIP address sanitizer: no nightly toolchain"
    echo "OK (sanitizer skipped)"
    exit 0
fi
# build.rs runs rustfmt on the generated opcode tables.
rustup component add --toolchain nightly rustfmt >/dev/null 2>&1 || true

echo "== address sanitizer probe"
PROBE_LOG=$(mktemp)
if ! RUSTFLAGS="-Zsanitizer=address" ASAN_OPTIONS=detect_leaks=0 \
        cargo +nightly test --target "$TARGET" --test jit sanitizer_probe \
        >"$PROBE_LOG" 2>&1; then
    echo "SKIP address sanitizer: the engine cannot create a context under ASan."
    echo "      Reproduces with no JIT attached (tests/jit/sanitizer_probe.rs), so it is"
    echo "      an engine/upstream limitation, not a JIT defect; recorded in"
    echo "      memo/jit_handover.md (M2b-8)."
    grep -E "panicked at|context: " "$PROBE_LOG" | head -3
    rm -f "$PROBE_LOG"
    echo "OK (sanitizer skipped, probe evidence above)"
    exit 0
fi
rm -f "$PROBE_LOG"

echo "== address sanitizer: jit suites (leaks off; the engine keeps process-lifetime atoms)"
RUSTFLAGS="-Zsanitizer=address" ASAN_OPTIONS=detect_leaks=0 \
    cargo +nightly test --target "$TARGET" --features jit --lib --tests \
    | grep -E "^test result" | sed 's/^/asan: /'

echo "OK"
