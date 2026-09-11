// D5: the loop must stay alive waiting for REAL external I/O (stdin). The module
// only completes once data arrives on fd 0. If the driver stopped the loop
// early (or dropped the future), this would hang or lose the result. The runner
// pipes data into stdin to satisfy it; without that, it must block, not exit 0.
import * as os from "os";
const v = await new Promise((resolve) => {
  os.setReadHandler(0, () => {
    os.setReadHandler(0, null);
    const ab = new ArrayBuffer(64);
    os.read(0, ab, 0, 64);
    resolve("AWAITED_STDIN");
  });
});
console.log(v);
