async function test() {
  console.log("before call");
  return 42;
}

let p = test();
p.then((v) => console.log("resolved:", v));
p.catch((e) => console.log("rejected:", e));
console.log("sync done");
