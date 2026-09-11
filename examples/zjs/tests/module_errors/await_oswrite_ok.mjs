// D4: a top-level await whose resolution is driven by the event loop (not a
// microtask). Verifies the loop genuinely spins and the await completes with a
// value rather than hanging or being short-circuited.
import * as os from "os";
const v = await new Promise((resolve) => {
  os.setWriteHandler(1, () => {
    os.setWriteHandler(1, null);
    resolve("AWAITED_OSWRITE");
  });
});
console.log(v);
