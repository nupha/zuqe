function f(a, b, c) {
  var x = 10;
  //log("a=" + a);
  function g(d) {
    function h() {
      log("d=" + d);
      log("x=" + x);
    }
    log("b=" + b);
    log("c=" + c);
    h();
  }
  g(4);
  return g;
}

var g1 = f(1, 2, 3);
g1(5);
