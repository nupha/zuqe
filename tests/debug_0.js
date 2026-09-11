"use strict";

function test_generator() {
  function* f() {
    var ret;
    yield 1;
    ret = yield 2;
    assert(ret, "next_arg");
    return 3;
  }
  function* f2() {
    yield 1;
    yield 2;
    return "ret_val";
  }
  function* f1() {
    var ret = yield* f2();
    assert(ret, "ret_val");
    return 3;
  }
  function* f3() {
    var ret;
    /* test stack consistency with nip_n to handle yield return +
     * finally clause */
    try {
      ret = 2 + (yield 1);
    } catch (e) {
    } finally {
      ret++;
    }
    return ret;
  }
  var g, v;
  g = f();
  v = g.next();
  assert(v.value === 1 && v.done === false);
  v = g.next();
  assert(v.value === 2 && v.done === false);
  v = g.next("next_arg");
  assert(v.value === 3 && v.done === true);
  v = g.next();
  assert(v.value === undefined && v.done === true);

  g = f1();
  v = g.next();
  assert(v.value === 1 && v.done === false);
  v = g.next();
  assert(v.value === 2 && v.done === false);
  v = g.next();
  assert(v.value === 3 && v.done === true);
  v = g.next();
  assert(v.value === undefined && v.done === true);

  g = f3();
  v = g.next();
  assert(v.value === 1 && v.done === false);
  v = g.next(3);
  assert(v.value === 6 && v.done === true);
}
