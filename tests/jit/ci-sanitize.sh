#!/usr/bin/env bash
# M2b-8: hardening matrix for the JIT.
#
# Legs:
#   1. dev build (debug_assertions on) — inline and background
#   2. release build (panic = abort, no unwinding)
#   3. fault injection: the compile pipeline must survive a refused mmap
#      (inline-only: the fault switch is `thread_local`)
#   4. ThreadSanitizer (nightly, `-Zbuild-std`): the worker/runtime threading
#   5. AddressSanitizer (nightly), gated on a probe that proves the engine can
#      create a context at all under the sanitizer
set -euo pipefail
cd "$(dirname "$0")/../.."

TARGET=x86_64-unknown-linux-gnu
# `default` minus `jit_background` (see tests/jit/ci-gate.sh): the inline model.
INLINE_FEATURES="inline_cache,big_int,all_unicode,async_support,jit,builtin_mapset,builtin_proxy,builtin_atomic,builtin_regexp,builtin_reflect,builtin_json,builtin_date,builtin_math,builtin_weakref"

echo "== dev (debug_assertions) — inline"
cargo test --no-default-features --features "$INLINE_FEATURES" --lib --tests | grep -E "^test result" | sed 's/^/inline: /'

echo "== dev (debug_assertions) — background (default)"
cargo test --lib --tests | grep -E "^test result" | sed 's/^/background: /'

echo "== release (panic=abort)"
cargo test --release --lib --tests | grep -E "^test result" | sed 's/^/release: /'

echo "== fault injection (refused mmap -> settle in the interpreter; inline)"
cargo test --no-default-features --features "$INLINE_FEATURES" --lib allocation_failure | grep -E "^test result" | sed 's/^/fault: /'

if ! rustup toolchain list 2>/dev/null | grep -q '^nightly'; then
    echo "== SKIP sanitizers: no nightly toolchain"
    echo "OK (sanitizers skipped)"
    exit 0
fi
# build.rs runs rustfmt on the generated opcode tables.
rustup component add --toolchain nightly rustfmt >/dev/null 2>&1 || true

# ThreadSanitizer. Unlike ASan this runs the engine (context creation is fine),
# so it is a real leg. It *needs* `-Zbuild-std` — `-Zsanitizer` changes the ABI,
# and without an instrumented std the build hard-errors on the mismatch — so it
# also needs the `rust-src` component. `rustup run` executes the toolchain's own
# cargo/rustc, so a local `cargo` shim (build cache) cannot break the
# `-Zbuild-std` invocation.
if ! rustup run nightly cargo --version >/dev/null 2>&1 \
    || ! rustup component list --toolchain nightly --installed 2>/dev/null | grep -q '^rust-src'; then
    echo "== SKIP thread sanitizer: nightly cargo or rust-src (-Zbuild-std) missing"
else
    echo "== thread sanitizer: jit suites (std instrumented via -Zbuild-std)"
    RUSTFLAGS="-Zsanitizer=thread" TSAN_OPTIONS="halt_on_error=0" \
        rustup run nightly cargo -Zbuild-std test --target "$TARGET" --features jit --lib --tests \
        | grep -E "^test result" | sed 's/^/tsan: /'
fi

echo "== address sanitizer probe"
PROBE_LOG=$(mktemp)
if ! RUSTFLAGS="-Zsanitizer=address" ASAN_OPTIONS=detect_leaks=0 \
        rustup run nightly cargo test --target "$TARGET" --test jit sanitizer_probe \
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
    rustup run nightly cargo test --target "$TARGET" --features jit --lib --tests \
    | grep -E "^test result" | sed 's/^/asan: /'

echo "OK"
