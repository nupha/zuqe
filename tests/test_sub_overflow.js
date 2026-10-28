// VM-P17 regression: i32-range subtraction must widen, not wrap.
//
// `TwoNumbers::sub` used raw i32 arithmetic, so `0 - -2147483648` wrapped to
// `-2147483648` in release builds and panicked under debug_assertions. The
// expectations below are hand-computed IEEE-754 doubles (verified against
// QuickJS 2026-06-04: prints `2147483648 -2147483649`).

var fails = 0;
function eq(got, want, label) {
    if (!Object.is(got, want)) {
        print("FAIL", label, "got", got, "want", want);
        fails++;
    }
}

// Subtraction crossing / exceeding the i32 range.
eq(0 - -2147483648, 2147483648, "0 - MIN");
eq(-2147483648 - 1, -2147483649, "MIN - 1");
eq(2147483647 - -1, 2147483648, "MAX - -1");
eq(2147483647 - -2147483648, 4294967295, "MAX - MIN");
eq(-2147483648 - 2147483647, -4294967295, "MIN - MAX");

// In-range results stay Int.
eq(5 - 3, 2, "5 - 3");
eq(-1 - -1, 0, "-1 - -1");
eq(0 - 0, 0, "0 - 0");

// Float and mixed paths unchanged.
eq(1.5 - 1, 0.5, "float sub");
eq(2147483648 - 1, 2147483647, "float lhs");
eq(true - false, 1, "bool sub");
eq("5" - 3, 2, "string coercion");

// Via locals / compound form (different bytecode shapes).
var lo = -2147483648;
eq(lo - 1, -2147483649, "local sub");
var acc = 0;
acc = acc - -2147483648;
eq(acc, 2147483648, "acc sub");

// Sub in a loop hitting the boundary repeatedly.
var s = 0;
for (var i = 0; i < 4; i++) s = s - 2147483648;
eq(s, -8589934592, "loop sub");

// Mul boundary sanity (regression guard for the shared fast path).
eq(-1 * 0, -0, "mul -0");
eq(100000 * 100000, 10000000000, "mul float");
eq(-2147483648 * 2, -4294967296, "mul MIN*2");

print(fails === 0 ? "PASS" : "FAIL " + fails);
