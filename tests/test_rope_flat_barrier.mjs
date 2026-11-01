// RopeImpl.flat memo write-barrier repro.
//
// `StringRef::flatten` memoises the materialised flat node into `rope.flat`
// with `alloc_local`. Once the allocating scope exits the node loses its LOCAL
// flag, so an OLD rope holding the only reference to that young flat node must
// bind it — otherwise the next minor frees the flat node and `rope.flat`
// dangles (validator panic / UAF on the next read).
//
// Sequence:
//   1. Build a rope (>ROPE_SHORT2_LEN so `+` does not memcpy) stored ONLY
//      inside an old object — not on the stack at minor time, so the minor
//      does not force-descend it.
//   2. Promote the holder + rope to the old generation.
//   3. Flatten the rope (slice -> sub_string -> flatten). The memoised flat
//      node is the only reference left after the call returns.
//   4. Minor: without the barrier the flat node is swept while the old rope
//      still points at it -> stale edge.
import * as std from "std";

function check(cond, msg) {
    if (!cond) throw new Error(msg);
}

function minor() {
    const before = std.gcStats().minorCount;
    for (let i = 0; i < 16 && std.gcStats().minorCount === before; i++) {
        std.gcMinor();
        for (let k = 0; k < 1024; k++) {
            if (k === -1) throw new Error("unreachable");
        }
    }
    check(std.gcStats().minorCount > before, "minor did not run");
}

std.gcPromote(1);

// A rope > ROPE_SHORT2_LEN (8192): "x"*9000 is flat, + "y" builds a rope node.
const holder = { s: "x".repeat(9000) + "y" };
check(typeof holder.s === "string", "rope not built");

minor();
minor(); // promote holder + rope to the old generation

// Flatten the rope. slice() -> sub_string() -> flatten() memoises the flat
// node into the (old) rope. The slice result is a fresh substring; the
// memoised flat node is then referenced ONLY by `rope.flat`.
const sub = holder.s.slice(0, 16);
check(sub === "x".repeat(16), "slice content mismatch");

// Drop the substring so nothing but `rope.flat` can reach the memo node.
// (Even if it survived, the next minor must not see a stale edge.)
minor();
minor();

// The rope must still be readable: a dangling `flat` memo would read freed
// memory here (and the debug validator already panicked at the minors above).
check(holder.s.length === 9001, "rope length changed");
check(holder.s[9000] === "y", "rope tail corrupted");
check(holder.s.slice(8990) === "x".repeat(10) + "y", "rope tail content corrupted");

std.puts("rope flat barrier: ok\n");
