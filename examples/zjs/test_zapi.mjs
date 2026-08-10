import "./zapi_demo.dylib";

console.log("=== Simple functions ===");
console.log("add(1, 2) =", add(1, 2));
console.log("mul(3, 4) =", mul(3, 4));
console.log("greet() =", greet());

console.log("\n=== Counter class ===");
let c = Counter("my-counter");
console.log("counter_value:", counter_value(c));
console.log("counter_increment:", counter_increment(c));
console.log("counter_increment:", counter_increment(c));
console.log("counter_value:", counter_value(c));
