function test(n) {
  var obj = {};
  var start = Date.now();
  for (var j = 0; j < n; j++) {
    obj[j] = 1;
  }
  var end = Date.now();
  return end - start;
}

for (var k = 1; k <= 20000; k = k * 2) {
  var t = test(k);
  print("n=" + k + " time=" + t + "ms per=" + (t * 1000) / k + "us");
}
