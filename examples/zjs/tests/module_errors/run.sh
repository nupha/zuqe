#!/usr/bin/env bash
# Regression runner for the zjs top-level module evaluation fix
# (examples/zjs/main.rs -> zuqe async Context::eval_wait).
#
# This is NOT a file-by-file copy of aike/tests/module_errors. It captures the
# SAME IDEA: a top-level module's evaluation outcome (value / sync error / async
# error / interrupt / "must block on a live promise") must flow truthfully to the
# process exit code, and the event loop and the top-level Promise must race
# without either side being dropped, hung, or cross-contaminated.
#
# zjs has no setTimeout, so "future-tick" scenarios are driven by the real event
# loop via `os.setWriteHandler` / `os.setReadHandler` (stdout is writable on the
# first poll; stdin is supplied by the runner). This is the zjs equivalent of
# aike's setTimeout-driven rejection/completion.
#
# Usage: bash examples/zjs/tests/module_errors/run.sh
set -u

cargo build --example zjs 2>/dev/null
ROOT="$(cd "$(dirname "$0")/../../../.." && echo "$PWD")"
ZJS="$ROOT/target/debug/examples/zjs"
DIR="$(cd "$(dirname "$0")" && echo "$PWD")"

if [[ ! -x "$ZJS" ]]; then
  echo "zjs binary not found at $ZJS — build failed?" >&2
  exit 2
fi

pass=0
fail=0
tmp="$(mktemp)"

# run_capture <file> <stdin_data> -> sets $code, $out
run_capture() {
  local file="$1" sin="${2:-}"
  if [[ -n "$sin" ]]; then
    out="$(printf '%s' "$sin" | "$ZJS" "$DIR/$file" 2>"$tmp")"
  else
    out="$("$ZJS" "$DIR/$file" 2>"$tmp")"
  fi
  code=$?
}

# check <file> <expected_exit> <marker> [stdin_data]
check() {
  local file="$1" expected="$2" want="${3:-}" sin="${4:-}"
  run_capture "$file" "$sin"
  local err; err="$(cat "$tmp")"
  local ok=1
  [[ "$code" == "$expected" ]] || ok=0
  if [[ -n "$want" && "$out$err" != *"$want"* ]]; then ok=0; fi
  if [[ "$ok" == 1 ]]; then
    echo "PASS  $file  (exit=$code${want:+, ~'$want'})"
    pass=$((pass+1))
  else
    echo "FAIL  $file  (exit=$code, expected=$expected${want:+, ~'$want'})"
    echo "      stdout: $out"
    echo "      stderr: $err"
    fail=$((fail+1))
  fi
}

# check_hang <file> <timeout_sec>
# The script must NOT exit 0; it must block until killed (correct: live Promise
# is not dropped). TIMED_OUT is the expected/correct outcome. Uses perl for a
# portable timeout (macOS lacks GNU `timeout`).
check_hang() {
  local file="$1" t="$2"
  local rc
  rc="$(printf '' | perl -e '
    my ($zjs, $file, $secs) = @ARGV;
    my $pid = fork;
    die "fork failed: $!" unless defined $pid;
    if ($pid == 0) { exec($zjs, $file) or die "exec failed: $!"; }
    local $SIG{ALRM} = sub { kill("KILL", $pid); waitpid($pid, 0); print "124\n"; exit; };
    alarm($secs);
    waitpid($pid, 0);
    alarm(0);
    print ($? >> 8), "\n";
  ' -- "$ZJS" "$DIR/$file" "$t" 2>/dev/null)"
  if [[ "$rc" == "124" ]]; then
    echo "PASS  $file  (correctly blocked / TIMED_OUT, live Promise not dropped)"
    pass=$((pass+1))
  else
    echo "FAIL  $file  (exited with rc=$rc — a live top-level Promise was dropped!)"
    echo "      stderr: $(cat "$tmp")"
    fail=$((fail+1))
  fi
}

# D1: synchronous / undefined-global errors
check sync_throw.mjs       1 SYNC_THROW_MARKER
check undef_global.mjs     1 "nonexistent_global_xyz_marker"

# D2: microtask rejection
check async_throw_microtask.mjs 1 ASYNC_THROW_MICROTASK_MARKER

# D3: rejection produced by the event-loop tick (core regression case)
check async_throw_oswrite.mjs 1 ASYNC_THROW_OSWRITE_MARKER

# D4: event-loop-driven await completes with a value
check await_oswrite_ok.mjs  0 AWAITED_OSWRITE

# D5: loop stays alive for real external I/O (stdin fed by runner)
check await_stdin.mjs       0 AWAITED_STDIN "feed-me-some-stdin"

# D6: explicit interrupt returns its exit code
check exit_immediate.mjs    7

# D7: a never-settling top-level Promise must block, NOT silently succeed
check_hang await_hang_guard.mjs 3

rm -f "$tmp"
echo "----"
echo "PASS=$pass FAIL=$fail"
[[ "$fail" == 0 ]]
