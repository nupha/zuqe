// D7 (cognitive counter-example, must HANG): a top-level await of a Promise that
// NEVER settles (we register a stdin read handler but resolve nothing). The
// correct driver (`select`, not `join!`) keeps the loop alive waiting for it, so
// the process must block — it must NOT silently exit 0 (which is what dropping
// the future via `join!` would have done). The runner enforces this with a
// timeout watchdog: TIMED_OUT is the CORRECT outcome here.
import * as os from "os";
await new Promise(() => {
  os.setReadHandler(0, () => {});
});
console.log("SHOULD_NOT_PRINT");
