// Test: class extends NativeClass prototype chain bug

// Test 1: extends Array (builtin native class)
class MyArray extends Array {
  foo() {
    return 3;
  }
  async bar() {
    return 4;
  }
}

const a = new MyArray();
print("=== Test 1: extends Array ===");
print("typeof a.foo:", typeof a.foo);
print("typeof a.bar:", typeof a.bar);
print("a.foo():", a.foo ? a.foo() : "N/A");
print(
  "Object.getPrototypeOf(a) === MyArray.prototype:",
  Object.getPrototypeOf(a) === MyArray.prototype,
);
print(
  "Object.getPrototypeOf(a) === Array.prototype:",
  Object.getPrototypeOf(a) === Array.prototype,
);

// Test 2: extends Error (builtin native class)
class MyError extends Error {
  hello() {
    return "world";
  }
}

const e = new MyError("test");
print("\n=== Test 2: extends Error ===");
print("typeof e.hello:", typeof e.hello);
print("e.hello():", e.hello ? e.hello() : "N/A");
print(
  "Object.getPrototypeOf(e) === MyError.prototype:",
  Object.getPrototypeOf(e) === MyError.prototype,
);
print(
  "Object.getPrototypeOf(e) === Error.prototype:",
  Object.getPrototypeOf(e) === Error.prototype,
);

// Test 3: plain class (should work)
class Plain {
  foo() {
    return 42;
  }
}
const p = new Plain();
print("\n=== Test 3: plain class (control) ===");
print("typeof p.foo:", typeof p.foo);
print("p.foo():", p.foo());
print(
  "Object.getPrototypeOf(p) === Plain.prototype:",
  Object.getPrototypeOf(p) === Plain.prototype,
);

// Test 4: subclass with own properties
class MyArray2 extends Array {
  constructor() {
    super();
    this.myProp = 99;
  }
  greet() {
    return "hello";
  }
}
const a2 = new MyArray2();
print("\n=== Test 4: extends Array with own properties ===");
print("a2.myProp:", a2.myProp);
print("typeof a2.greet:", typeof a2.greet);
print("a2.greet():", a2.greet ? a2.greet() : "N/A");
print(
  "Object.getPrototypeOf(a2) === MyArray2.prototype:",
  Object.getPrototypeOf(a2) === MyArray2.prototype,
);

// Test 5: extends user-defined native class (Emitter)
// This is the KEY test for the bug report
class MyEmitter extends Emitter {
  foo() {
    return 3;
  }
  async bar() {
    return 4;
  }
}

const em = new MyEmitter();
print("\n=== Test 5: extends Emitter (user-defined native class) ===");
print("typeof em.foo:", typeof em.foo);
print("typeof em.bar:", typeof em.bar);
print("em.foo():", em.foo ? em.foo() : "N/A");
print(
  "Object.getPrototypeOf(em) === MyEmitter.prototype:",
  Object.getPrototypeOf(em) === MyEmitter.prototype,
);
print(
  "Object.getPrototypeOf(em) === Emitter.prototype:",
  Object.getPrototypeOf(em) === Emitter.prototype,
);

// Test 6: extends Emitter with own properties and super()
class MyEmitter2 extends Emitter {
  constructor() {
    super();
    this.myProp = 42;
  }
  greet() {
    return "hello";
  }
}
const em2 = new MyEmitter2();
print("\n=== Test 6: extends Emitter with own properties ===");
print("em2.myProp:", em2.myProp);
print("typeof em2.greet:", typeof em2.greet);
print("em2.greet():", em2.greet ? em2.greet() : "N/A");
print("typeof em2.on:", typeof em2.on);
print("typeof em2.emit:", typeof em2.emit);
print(
  "Object.getPrototypeOf(em2) === MyEmitter2.prototype:",
  Object.getPrototypeOf(em2) === MyEmitter2.prototype,
);

// Test 7: native methods still work after extends
em2.on("test");
print("\n=== Test 7: native methods after extends ===");
print("em2.emit('test'):", em2.emit("test"));
