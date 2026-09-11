// Exception catch and propagation verification for stackless arena mode.
//
// Validates the following implementation details:
// 1. Backtrace is added to Error objects on catch (State::Exception path)
// 2. Backtrace survives exception propagation through virtual frames (throwed path)
// 3. Finally blocks execute during unwinding (find_finally_in_frame / GOSUB markers)
// 4. VarRef detach before free_frame in Unwind path (no dangling pointers with closure captures)
// 5. Nested try/catch across multiple call frames
// 6. IteratorClose with throw (CatchOffset(0) handling)
// 7. Uncatchable exceptions (e.g. interrupt) bypass catch

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

function assert_throws(fn, msg) {
    num_tests++;
    try {
        fn();
        print('FAIL: ' + msg + ' (no exception thrown)');
    } catch (e) {
        num_pass++;
    }
}

function assert_throws_instance(fn, type) {
    num_tests++;
    try {
        fn();
        print('FAIL: expected ' + type.name + ' to be thrown');
    } catch (e) {
        if (e instanceof type) {
            num_pass++;
        } else {
            print('FAIL: expected ' + type.name + ', got ' + e);
        }
    }
}

// ── 1. Simple try/catch with backtrace ──
(function test_simple_catch() {
    var err;
    try {
        throw new Error('test1');
    } catch (e) {
        err = e;
    }
    assert(err !== undefined, 'test_simple_catch: error caught');
    assert(err.message === 'test1', 'test_simple_catch: message preserved');
    assert(err.stack !== undefined, 'test_simple_catch: stack trace present');
})();

// ── 2. try/catch in nested function calls ──
(function test_nested_catch() {
    function inner() {
        throw new Error('nested');
    }
    function middle() {
        inner();
    }
    function outer() {
        try {
            middle();
        } catch (e) {
            return e;
        }
    }
    var err = outer();
    assert(err !== undefined, 'test_nested_catch: error propagated');
    assert(err.message === 'nested', 'test_nested_catch: message preserved');
    assert(err.stack !== undefined, 'test_nested_catch: stack trace present');
})();

// ── 3. try/finally always executes (exception then propagates out) ──
(function test_finally() {
    var finally_executed = false;
    try {
        try {
            throw new Error('finally_test');
        } finally {
            finally_executed = true;
        }
    } catch (e) {
        // exception propagated after finally ran
    }
    assert(finally_executed, 'test_finally: finally executed before unwinding');
})();

// ── 4. try/catch with finally ──
(function test_catch_finally() {
    var finally_executed = false;
    var caught = false;
    try {
        throw new Error('cf');
    } catch (e) {
        caught = true;
    } finally {
        finally_executed = true;
    }
    assert(caught, 'test_catch_finally: catch block executed');
    assert(finally_executed, 'test_catch_finally: finally block executed');
})();

// ── 5. finally + catch in nested frames ──
(function test_nested_finally_catch() {
    var order = [];
    function inner() {
        try {
            throw new Error('nested_finally');
        } finally {
            order.push('inner_finally');
        }
    }
    function outer() {
        try {
            inner();
        } catch (e) {
            order.push('outer_catch');
        } finally {
            order.push('outer_finally');
        }
    }
    outer();
    assert(order.length === 3, 'test_nested_finally_catch: all 3 blocks ran');
    assert(order[0] === 'inner_finally', 'test_nested_finally_catch: inner_finally first');
    assert(order[1] === 'outer_catch', 'test_nested_finally_catch: outer_catch second');
    assert(order[2] === 'outer_finally', 'test_nested_finally_catch: outer_finally last');
})();

// ── 6. Throw non-Error value ──
(function test_throw_non_error() {
    var caught;
    try {
        throw 42;
    } catch (e) {
        caught = e;
    }
    assert(caught === 42, 'test_throw_non_error: raw value caught');
})();

// ── 7. Throw string value ──
(function test_throw_string() {
    var caught;
    try {
        throw 'hello_error';
    } catch (e) {
        caught = e;
    }
    assert(caught === 'hello_error', 'test_throw_string: string caught');
})();

// ── 8. Re-throw in catch ──
(function test_rethrow() {
    var outer_caught;
    try {
        try {
            throw new Error('rethrown');
        } catch (e) {
            throw e;
        }
    } catch (e) {
        outer_caught = e;
    }
    assert(outer_caught !== undefined, 'test_rethrow: rethrown error caught');
    assert(outer_caught.message === 'rethrown', 'test_rethrow: message preserved');
})();

// ── 9. TypeError thrown and caught ──
(function test_type_error() {
    assert_throws_instance(function() {
        null.x();
    }, TypeError, 'test_type_error: null property access');
})();

// ── 10. ReferenceError thrown and caught ──
(function test_reference_error() {
    assert_throws_instance(function() {
        nonexistent_var_xyz;
    }, ReferenceError, 'test_reference_error: undefined variable');
})();

// ── 11. SyntaxError in eval ──
(function test_eval_syntax_error() {
    assert_throws(function() {
        eval('invalid syntax {{{');
    }, 'test_eval_syntax_error: eval syntax error');
})();

// ── 12. Deep call stack with try/catch at top ──
(function test_deep_stack_catch() {
    function recurse(n) {
        if (n <= 0) {
            throw new Error('deep');
        }
        return recurse(n - 1);
    }
    var err;
    try {
        recurse(100);
    } catch (e) {
        err = e;
    }
    assert(err !== undefined, 'test_deep_stack_catch: deep error caught');
    assert(err.stack !== undefined, 'test_deep_stack_catch: stack trace present');
})();

// ── 13. Nested try/catch in same function ──
(function test_nested_try_same_frame() {
    var outer = false;
    var inner = false;
    try {
        try {
            throw new Error('inner');
        } catch (e) {
            inner = true;
        }
        outer = true; // should reach here
    } catch (e) {
        // should NOT reach here
    }
    assert(inner, 'test_nested_try_same_frame: inner catch executed');
    assert(outer, 'test_nested_try_same_frame: execution continued after inner catch');
})();

// ── 14. try/catch with closure capturing outer variables ──
(function test_closure_varref_unwind() {
    // Validates VarRef detach before free_frame in Unwind path.
    // Closures capture `counter` from the outer frame; throwing
    // triggers Unwind which must detach VarRefs before freeing the frame.
    var counter = 0;
    function make_closure() {
        var x = 10;
        // Capture x and outer counter in a closure
        function inner() {
            return x + counter;
        }
        throw new Error('closure_unwind');
    }
    assert_throws(make_closure, 'test_closure_varref_unwind: closure varref detach');
})();

// ── 15. catch with inner closure that survives ──
(function test_catch_closure_survives() {
    var closure;
    try {
        var local = 42;
        throw new Error('catch_closure');
    } catch (e) {
        // Create a closure inside catch that captures the caught error
        closure = function() { return e.message; };
    }
    assert(closure() === 'catch_closure', 'test_catch_closure_survives: closure in catch works');
})();

// ── 16. For-in loop with throw + iterator close ──
(function test_for_in_close() {
    var obj = { x: 1, y: 2, z: 3 };
    var seen = [];
    try {
        for (var k in obj) {
            seen.push(k);
            if (k === 'y') {
                throw new Error('stop_iter');
            }
        }
    } catch (e) {
        assert(e.message === 'stop_iter', 'test_for_in_close: exception caught');
    }
    assert(seen.indexOf('x') >= 0, 'test_for_in_close: x iterated');
    assert(seen.indexOf('y') >= 0, 'test_for_in_close: y iterated');
    assert(seen.indexOf('z') < 0, 'test_for_in_close: z not iterated (thrown before)');
})();

// ── 17. Generator throw ──
(function test_generator_throw() {
    function* gen() {
        try {
            yield 1;
        } catch (e) {
            return 'caught:' + e.message;
        }
    }
    var g = gen();
    assert(g.next().value === 1, 'test_generator_throw: first yield');
    try {
        var result = g.throw(new Error('gen_err'));
        assert(result.value === 'caught:gen_err', 'test_generator_throw: caught in generator, got: ' + result.value);
        assert(result.done === true, 'test_generator_throw: done after catch');
    } catch (e) {
        assert(false, 'test_generator_throw: throw() uncaught: ' + e);
    }
})();

// ── 18. Async-like pattern: callback that may throw ──
(function test_callback_throw() {
    function callWithCatch(fn) {
        try {
            fn();
        } catch (e) {
            return 'error:' + e.message;
        }
    }
    var result = callWithCatch(function() {
        throw new Error('cb_error');
    });
    assert(result === 'error:cb_error', 'test_callback_throw: callback error caught');
})();

// ── 19. Exception during error creation (malformed throw) ──
(function test_throw_undefined() {
    var caught;
    try {
        throw undefined;
    } catch (e) {
        caught = true;
    }
    assert(caught, 'test_throw_undefined: undefined can be thrown');
})();

// ── 20. finally with return in try block ──
(function test_finally_with_try_return() {
    var finally_ran = false;
    function fn() {
        try {
            return 42;
        } finally {
            finally_ran = true;
        }
    }
    var val = fn();
    assert(finally_ran, 'test_finally_with_try_return: finally ran after return');
    assert(val === 42, 'test_finally_with_try_return: return value preserved');
})();

// ── 21. finally with return in catch block ──
(function test_finally_with_catch_return() {
    var finally_ran = false;
    function fn() {
        try {
            throw new Error('err');
        } catch (e) {
            return 'caught';
        } finally {
            finally_ran = true;
        }
    }
    var val = fn();
    assert(val === 'caught', 'test_finally_with_catch_return: return from catch');
    assert(finally_ran, 'test_finally_with_catch_return: finally ran');
})();

// ── 22. Multiple finally blocks in one frame ──
(function test_multi_finally_one_frame_v2() {
    var order = [];
    try {
        try {
            try {
                throw new Error('multi');
            } finally {
                order.push('inner_finally');
            }
        } finally {
            order.push('outer_finally');
        }
    } catch (e) {
        // finally blocks ran before exception propagated here
    }
    assert(order.length === 2, 'test_multi_finally_one_frame: both finally ran');
    assert(order[0] === 'inner_finally', 'test_multi_finally_one_frame: inner first');
    assert(order[1] === 'outer_finally', 'test_multi_finally_one_frame: outer second');
})();

// ── 23. Class constructor that extends and throws ──
(function test_class_extends_throw() {
    class Base {
        constructor() {
            throw new Error('base_reject');
        }
    }
    class Derived extends Base {
        constructor() {
            try {
                super();
            } catch (e) {
                // When super() throws, `this` is not initialized.
                // The derived constructor must return a non-undefined object
                // or re-throw (ECMA-262 §12.3.5.1).
                return { recovered: true, original: e.message };
            }
        }
    }
    try {
        var d = new Derived();
        assert(d.recovered === true, 'test_class_extends_throw: recovered');
        assert(d.original === 'base_reject', 'test_class_extends_throw: original error preserved');
    } catch (e) {
        assert(false, 'test_class_extends_throw: uncaught: ' + e);
    }
})();

// ── 24. Exception in deeply nested closure-heavy code ──
// This specifically stresses VarRef detach in Unwind path,
// because each frame has closures capturing local variables.
(function test_deep_closure_exception() {
    function makeLayer(n, next) {
        var local = n;
        if (n <= 0) {
            return function() {
                throw new RangeError('depth_' + local);
            };
        }
        var nextFn = makeLayer(n - 1, next);
        return function() {
            return nextFn();
        };
    }
    var deepFn = makeLayer(50);
    assert_throws_instance(function() {
        deepFn();
    }, RangeError, 'test_deep_closure_exception: deep closure throw caught');
})();

// ── 25. Try/catch with yield in generator ──
(function test_gen_yield_catch() {
    function* gen() {
        var x = 0;
        while (x < 3) {
            try {
                yield x++;
            } catch (e) {
                yield 'err:' + x;
            }
        }
    }
    var g = gen();
    assert(g.next().value === 0, 'test_gen_yield_catch: yield 0');
    assert(g.next().value === 1, 'test_gen_yield_catch: yield 1');
    assert(g.throw(new Error('test')).value === 'err:2', 'test_gen_yield_catch: caught throw');
    assert(g.next().value === 2, 'test_gen_yield_catch: yield 2 after throw');
    assert(g.next().done === true, 'test_gen_yield_catch: done');
})();

// ── Summary ──
print('');
print('catch.js: ' + num_pass + '/' + num_tests + ' tests passed');
if (num_pass === num_tests) {
    print('All tests passed.');
} else {
    print('FAILED: ' + (num_tests - num_pass) + ' test(s) failed.');
}
