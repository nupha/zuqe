// Comprehensive test for extends NativeClass fix

// === Test 1: Basic extends ===
class MyArray1 extends Array {
  first() {
    return this[0];
  }
}
const a1 = new MyArray1(1, 2, 3);
print("Test 1 - MyArray1 extends Array:");
print("  a1.first():", a1.first());
print("  a1 instanceof MyArray1:", a1 instanceof MyArray1);
print("  a1 instanceof Array:", a1 instanceof Array);

// === Test 2: extends Error with constructor ===
class MyError1 extends Error {
  constructor(msg, code) {
    super(msg);
    this.code = code;
  }
  toString() {
    return `${this.name}: ${this.message} (code: ${this.code})`;
  }
}
const e1 = new MyError1("test error", 42);
print("\nTest 2 - MyError1 extends Error:");
print("  e1.message:", e1.message);
print("  e1.code:", e1.code);
print("  e1.toString():", e1.toString());
print("  e1 instanceof MyError1:", e1 instanceof MyError1);

// === Test 3: extends Emitter with methods ===
class MyEmitter extends Emitter {
  onAndEmit(event) {
    this.on(event);
    return this.emit(event);
  }
}
const em = new MyEmitter();
print("\nTest 3 - MyEmitter extends Emitter:");
print("  em.onAndEmit('click'):", em.onAndEmit("click"));
print("  em.emit('click'):", em.emit("click"));
print("  em instanceof MyEmitter:", em instanceof MyEmitter);
print("  em instanceof Emitter:", em instanceof Emitter);

// === Test 4: getter/setter on subclass ===
class MyArray2 extends Array {
  get last() {
    return this[this.length - 1];
  }
}
const a2 = new MyArray2(10, 20, 30);
print("\nTest 4 - getter on subclass:");
print("  a2.last:", a2.last);
print("  a2 instanceof MyArray2:", a2 instanceof MyArray2);

// === Test 5: static method ===
class MyArray3 extends Array {
  static fromRange(n) {
    const a = new MyArray3();
    for (let i = 0; i < n; i++) a.push(i);
    return a;
  }
}
print("\nTest 5 - static method:");
print("  MyArray3.fromRange(5):", MyArray3.fromRange(5));
print(
  "  MyArray3.fromRange(5) instanceof MyArray3:",
  MyArray3.fromRange(5) instanceof MyArray3,
);

// === Test 6: no constructor subclass ===
class MyEmitter2 extends Emitter {}
const em2 = new MyEmitter2();
print("\nTest 6 - no constructor subclass:");
print("  typeof em2.on:", typeof em2.on);
print("  typeof em2.emit:", typeof em2.emit);
print("  em2 instanceof MyEmitter2:", em2 instanceof MyEmitter2);
print("  em2 instanceof Emitter:", em2 instanceof Emitter);

// === Test 7: subclass with own property in constructor ===
class MyEmitter3 extends Emitter {
  constructor(name) {
    super();
    this.name = name;
  }
  greet() {
    return `Hello from ${this.name}`;
  }
}
const em3 = new MyEmitter3("World");
print("\nTest 7 - own property in constructor:");
print("  em3.name:", em3.name);
print("  em3.greet():", em3.greet());
print("  em3 instanceof MyEmitter3:", em3 instanceof MyEmitter3);

// === Summary ===
print("\n=== All tests completed ===");
