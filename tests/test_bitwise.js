// Bitwise operator regression: `& | ^` must follow ECMA-262 ToInt32 /
// ToUint32 semantics (truncate toward zero, reduce modulo 2^32, then
// reinterpret as signed) — see VM-B1 in memo/vm_todo.md.
//
// Two independent oracles are used so a bug in the engine's own bitwise path
// cannot mask itself:
//   1. a hardcoded expectation table (values from the spec / python3 ToInt32);
//   2. a runtime reference implemented with arithmetic only (`%`, `/`, `*`,
//      `Math.floor`), which never uses `& | ^`.
// Run: cargo run --release --example zjs -- tests/test_bitwise.js

function assert(actual, expected, message) {
    if (actual === expected) return;
    throw Error("assertion failed: got |" + actual + "|, expected |" +
                expected + "|" + (message ? " (" + message + ")" : ""));
}

// ── oracle 2: arithmetic-only ToInt32 / ToUint32 and bit combinators ────────
function refToUint32(x) {
    var n = Number(x);
    if (n !== n || !isFinite(n)) return 0;        // NaN / +-Infinity -> 0
    n = n < 0 ? -Math.floor(-n) : Math.floor(n);  // truncate toward zero
    var m = n % 4294967296;
    if (m < 0) m += 4294967296;
    return m;
}

function u32BitOp(a, b, keep) {   // keep: 0=and slot, 1=or slot, 2=xor slot
    var r = 0, bit = 1, i, ba, bb;
    for (i = 0; i < 32; i++) {
        ba = a % 2;
        bb = b % 2;
        if ((keep === 1 && (ba === 1 || bb === 1)) ||
            (keep === 0 && (ba === 1 && bb === 1)) ||
            (keep === 2 && (ba === 1) !== (bb === 1))) {
            r += bit;
        }
        a = (a - ba) / 2;
        b = (b - bb) / 2;
        bit *= 2;
    }
    return r;
}

function refOp(a, b, keep) {
    var u = u32BitOp(refToUint32(a), refToUint32(b), keep);
    return u >= 2147483648 ? u - 4294967296 : u;
}

function test_table() {
    // `|` — negatives, >=2^32, +/-Infinity, fractional operands.
    assert((-1) | 0, -1, "(-1)|0");
    assert(4294967296 | 0, 0, "4294967296|0");
    assert(2147483648 | 0, -2147483648, "2147483648|0");
    assert((-2147483649) | 0, 2147483647, "-2147483649|0");
    assert(4294967295 | 0, -1, "4294967295|0");
    assert((-2147483648) | 0, -2147483648, "-2147483648|0");
    assert(4294967297 | 0, 1, "4294967297|0");
    assert((-4294967296) | 0, 0, "-4294967296|0");
    assert(3.7 | 0, 3, "3.7|0");
    assert((-3.7) | 0, -3, "-3.7|0");
    assert(2147483647.9 | 0, 2147483647, "2147483647.9|0");
    assert((-2147483648.9) | 0, -2147483648, "-2147483648.9|0");
    assert(NaN | 0, 0, "NaN|0");
    assert(Infinity | 0, 0, "Infinity|0");
    assert((-Infinity) | 0, 0, "-Infinity|0");

    // `&`
    assert(4294967296 & (-1), 0, "4294967296&-1");
    assert(2147483648 & (-1), -2147483648, "2147483648&-1");
    assert((-1) & 4294967295, -1, "-1&4294967295");
    assert(3000000000 & (-1), -1294967296, "3000000000&-1");
    assert(2147483648 & 2147483648, -2147483648, "2147483648&2147483648");
    assert((-1) & (-1), -1, "-1&-1");

    // `^`
    assert((-1) ^ 0, -1, "(-1)^0");
    assert(2147483649 ^ 0, -2147483647, "2147483649^0");
    assert(4294967296 ^ 0, 0, "4294967296^0");
    assert((-1) ^ (-1), 0, "-1^-1");
    assert(2147483648 ^ 2147483648, 0, "2147483648^2147483648");
    assert((-2147483649) ^ 0, 2147483647, "-2147483649^0");
    assert(4294967295 ^ 4294967295, 0, "4294967295^4294967295");

    // shift family — already correct; locked in so a refactor cannot regress it.
    assert((-1) << 0, -1, "(-1)<<0");
    assert(2147483648 << 0, -2147483648, "2147483648<<0");
    assert((-1) >> 0, -1, "(-1)>>0");
    assert((-1) >>> 0, 4294967295, "(-1)>>>0");
    assert(4294967296 >>> 0, 0, "4294967296>>>0");
    assert(2147483648 >>> 1, 1073741824, "2147483648>>>1");
    assert((-2147483649) >> 0, 2147483647, "-2147483649>>0");
    assert(4294967297 >>> 0, 1, "4294967297>>>0");
}

function test_compound() {
    var r;
    r = -1; r |= 0;            assert(r, -1, "r|=");
    r = 2147483648; r ^= 0;    assert(r, -2147483648, "r^=");
    r = 4294967296; r &= -1;   assert(r, 0, "r&=");
    r = 4294967295; r &= 1;    assert(r, 1, "r&=1");
    r = -2147483649; r |= 0;   assert(r, 2147483647, "r|=2");
}

function test_recurrence() {
    // The classic `hash = (2*hash + c) | 0` pattern used to deadlock at
    // 4294967295 once the accumulator became a Float64.
    var s = 0, i;
    for (i = 0; i < 40; i++) s = (2 * s + i) | 0;
    assert(s, -41, "(2*s+i)|0 recurrence");

    var t = 0;
    for (i = 0; i < 40; i++) t = ((t << 1) | i) | 0;
    assert(t, -1, "((s<<1)|i)|0 recurrence");
}

function test_differential() {
    // Independent oracle over a spread of interesting operands.
    var vals = [
        -2147483649, -2147483648, -2147483647, -4294967296, -4294967295,
        -1, -0.5, 0, 0.5, 1, 2, 3, 7, 31, 255, 1024, 65535,
        2147483646, 2147483647, 2147483648, 2147483649,
        3000000000, 4294967294, 4294967295, 4294967296, 4294967297,
        8589934591, 8589934592, 8589934593,
        -8589934593, -3000000000, -2147483648.9, 2147483647.9,
        NaN, Infinity, -Infinity,
    ];
    var i, j, a, b;
    for (i = 0; i < vals.length; i++) {
        for (j = 0; j < vals.length; j++) {
            a = vals[i]; b = vals[j];
            assert(a | b, refOp(a, b, 1), "diff | " + a + "," + b);
            assert(a & b, refOp(a, b, 0), "diff & " + a + "," + b);
            assert(a ^ b, refOp(a, b, 2), "diff ^ " + a + "," + b);
        }
    }
}

test_table();
test_compound();
test_recurrence();
test_differential();
console.log('test_bitwise.js: all tests passed');
