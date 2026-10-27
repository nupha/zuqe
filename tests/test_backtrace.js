// VM-P15 regression: error objects and their `stack` text survive the
// backtrace-builder rewrite (no per-frame allocations, single String buffer).
//
// The stack format is the engine contract:
//   "    at <name> (<filename>[:line:col])\n" per frame, trailing newline.
// `new Error()` skips the Error constructor itself (SKIP_FIRST_LEVEL).

var num_tests = 0;
var num_pass = 0;

function assert(cond, msg) {
    num_tests++;
    if (cond) {
        num_pass++;
    } else {
        print('FAIL: ' + msg);
    }
}

// ── 1. Named function frames, in call order, constructor skipped ──
function inner() {
    return new Error('boom');
}
function outer() {
    return inner();
}
var err = outer();

assert(err.name === 'Error', 'name preserved');
assert(err.message === 'boom', 'message preserved');
assert(typeof err.stack === 'string', 'stack is a string');

var lines = err.stack.split('\n');
assert(lines.length === 4, 'Error stack has 3 frames + trailing newline, got ' + lines.length);
assert(lines[0].indexOf('    at inner (') === 0, 'first frame is inner: ' + lines[0]);
assert(lines[1].indexOf('    at outer (') === 0, 'second frame is outer: ' + lines[1]);
assert(lines[2].indexOf('    at <eval> (') === 0, 'third frame is <eval>: ' + lines[2]);
assert(lines[3] === '', 'stack ends with a newline');
assert(lines[0].indexOf('test_backtrace.js') > 0, 'frame names the source file');
assert(lines[1].indexOf('test_backtrace.js') > 0, 'frame names the source file');

// ── 2. `stack` stays a plain writable/configurable data property ──
var desc = Object.getOwnPropertyDescriptor(err, 'stack');
assert(desc !== undefined, 'stack is an own property');
assert(desc.value === err.stack, 'stack descriptor value matches');
assert(desc.writable === true && desc.configurable === true, 'stack flags unchanged');
assert(desc.enumerable === false, 'stack stays non-enumerable');

// ── 3. Re-throwing a materialised error does not rewrite its stack ──
var original_stack = err.stack;
try {
    throw err;
} catch (again) {
    assert(again === err, 're-throw keeps identity');
    assert(again.stack === original_stack, 're-throw keeps stack text');
}

// ── 4. Native-thrown TypeError: message + throw-site frame with line:col ──
try {
    null.property;
} catch (e) {
    assert(e instanceof TypeError, 'null access throws TypeError');
    assert(e.message === 'cannot read property of null', 'TypeError message');
    assert(typeof e.stack === 'string' && e.stack.length > 0, 'TypeError has stack');
    var tl = e.stack.split('\n');
    assert(tl.length === 2, 'TypeError stack has one frame, got ' + tl.length);
    assert(tl[0].indexOf('    at <eval> (') === 0, 'TypeError frame is <eval>: ' + tl[0]);
    assert(/:\d+:\d+\)$/.test(tl[0]), 'TypeError frame has line:col: ' + tl[0]);
    assert(tl[1] === '', 'TypeError stack ends with a newline');
}

// ── 5. Multi-frame stack keeps every frame and its order ──
function f5() { return new RangeError('deep'); }
function f4() { return f5(); }
function f3() { return f4(); }
function f2() { return f3(); }
function f1() { return f2(); }
var deep = f1().stack.split('\n');
assert(deep.length === 7, '5 frames + eval + trailing newline, got ' + deep.length);
assert(deep[0].indexOf('    at f5 (') === 0, 'deep frame f5');
assert(deep[1].indexOf('    at f4 (') === 0, 'deep frame f4');
assert(deep[2].indexOf('    at f3 (') === 0, 'deep frame f3');
assert(deep[3].indexOf('    at f2 (') === 0, 'deep frame f2');
assert(deep[4].indexOf('    at f1 (') === 0, 'deep frame f1');

// ── 6. Anonymous functions and methods keep their names ──
var arrow = () => new Error('arrow');
var arrow_line = arrow().stack.split('\n')[0];
assert(arrow_line.indexOf('    at arrow (') === 0, 'arrow name: ' + arrow_line);

var holder = {
    method: function () { return new Error('method'); },
};
var method_line = holder.method().stack.split('\n')[0];
assert(method_line.indexOf('    at method (') === 0, 'method name: ' + method_line);

print('');
print('test_backtrace.js: ' + num_pass + '/' + num_tests + ' tests passed');
if (num_pass === num_tests) {
    print('All tests passed.');
} else {
    print('FAILED: ' + (num_tests - num_pass) + ' test(s) failed.');
}
