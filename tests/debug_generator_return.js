var inTry = 0;
var unreachable = 0;
function* g() {
  try {
    inTry += 1;
    yield;
    unreachable += 1;
  } catch (e) {
    throw e;
  }
  unreachable += 1;
}
var iter = g();

var result = iter.next();
console.log("inTry:", inTry, "(expected 1)");

result = iter.return(44);
console.log("return value:", result.value, "(expected 44)");
console.log("return done:", result.done, "(expected true)");
console.log("unreachable:", unreachable, "(expected 0)");

result = iter.next();
console.log("next value:", result.value, "(expected undefined)");
console.log("next done:", result.done, "(expected true)");
console.log("unreachable:", unreachable, "(expected 0)");
