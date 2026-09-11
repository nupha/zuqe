// D3 (CORE): the rejection is produced by the EVENT LOOP tick, not a microtask.
// `os.setWriteHandler(1, cb)` fires `cb` on the first `js_os_poll` (stdout is
// writable), so the rejection settles only after the loop has actually spun.
// This is the zjs equivalent of aike's `setTimeout`-driven rejection and is the
// exact case the old `tokio::join!` driver dropped (loop stops first -> future
// dropped -> silent success). Correct behavior: exit 1 with the marker.
import * as os from "os";
await new Promise((_, reject) => {
  os.setWriteHandler(1, () => {
    os.setWriteHandler(1, null); // unregister so the loop can stop
    reject(new Error("ASYNC_THROW_OSWRITE_MARKER"));
  });
});
