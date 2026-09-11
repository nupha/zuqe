//! Quick smoke test for the zapi_event example. The comprehensive suite is
//! tests/zapi/test_zapi_event.mjs.
//!
//! Run after building the extension:
//!   cargo build --release --manifest-path examples/zapi_event/Cargo.toml
//!   target/debug/examples/zjs --module examples/zapi_event/test_event.mjs

import { EventEmitter } from "./target/release/libzapi_event.dylib";

let failures = 0;
function check(name, cond) {
  if (cond) {
    console.log("PASS " + name);
  } else {
    console.log("FAIL " + name);
    failures++;
  }
}

const ee = new EventEmitter();

// Multi-handler dispatch in order; emit returns true.
let calls = [];
ee.on("ping", () => calls.push("a"));
ee.on("ping", () => calls.push("b"));
check("emit returns true", ee.emit("ping") === true);
check("handlers called in order", calls.join(",") === "a,b");
check("emit returns false without listener", ee.emit("nope") === false);

// once fires exactly once.
let once = 0;
ee.once("tick", () => once++);
ee.emit("tick");
ee.emit("tick");
check("once fires once", once === 1);

// off removes a specific handler.
let n = 0;
const fa = () => n++;
const fb = () => (n += 10);
ee.on("x", fa);
ee.on("x", fb);
ee.off("x", fa);
ee.emit("x");
check("off removes specific handler", n === 10);

console.log(failures === 0 ? "ALL PASS" : failures + " FAILURES");
