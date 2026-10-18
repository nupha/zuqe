#!/usr/bin/env python3
"""M2b-2 claim audit: check that every opcode a case claims really appears in
the bytecode the engine prints for that case.

    cargo test --features jit,dump_bytecode --test jit bytecode_dump_audit \
        -- --nocapture > /tmp/opcodes_dump.txt
    tests/jit/opcode-claims.py /tmp/opcodes_dump.txt          # check
    tests/jit/opcode-claims.py --fix /tmp/opcodes_dump.txt    # rewrite claims

The dump interleaves a case's scripts and functions in one `<Bytecode>` block,
so a claim describes the whole case (its scripts included). `CLAIM` lines come
from `bytecode_dump_audit` and list what the manifest claims. Only the final
(pass 2) bytecode is scanned: an opcode that disappears before resolution
(`resolve_variables`/`resolve_labels`) does not count as covered.
"""
import collections
import re
import sys
from pathlib import Path

TESTS = Path(__file__).resolve().parent / "jit_opcodes.rs"


def split_blocks(lines):
    """Group the dump into one block per function.

    The engine prints a function's bytecode *before* its `<Bytecode …>` header
    (the header is emitted when the bytecode object is created, after both
    passes), so a block ends at its header — attributing the preceding body to
    the right function instead of the next one.
    """
    blocks, cur = [], []
    for ln in lines:
        cur.append(ln)
        if ln.startswith("<Bytecode "):
            blocks.append(cur)
            cur = []
    if cur:
        blocks.append(cur)
    return blocks


OP_LINE = re.compile(r"^\s*\d+\s+([0-9A-F]{2}(\s+[0-9A-F]{2})*)\s+(?:\d+:\s*)?([a-z0-9_]+)\b")
CONT = re.compile(r"^\s+([0-9A-F]{2})\s+(?:\d+:\s*)?([a-z0-9_]+)\b")
STACK = re.compile(r"^\s*\d+:\s+([a-z0-9_]+)\b")
HEADER = re.compile(r"(audit-[^:]+)\.js:\d+: function: (\S+)")
CLAIM = re.compile(r"^CLAIM (\S+) (.*)$")
PASS = re.compile(r"^pass (\d+)$")
SECOND_PASS = 2


def final_pass_lines(lines):
    """Keep only the second (final) pass of every dumped function.

    The engine dumps pass 1 (pre-resolution) and pass 2 (post-resolution, what
    the JIT consumes). A claim that only appears before variable/label
    resolution must not pass the audit, so every pass-1 block is dropped.
    """
    kept = []
    pass_no = None
    for ln in lines:
        m = PASS.match(ln)
        if m:
            pass_no = int(m.group(1))
            continue
        if pass_no is None or pass_no == SECOND_PASS:
            kept.append(ln)
    return kept


def found_in_dump(text, nameset):
    found = collections.defaultdict(set)
    for block in split_blocks(final_pass_lines(text.splitlines())):
        header = next((ln for ln in reversed(block) if ln.startswith("<Bytecode ")), "")
        m = HEADER.search(header)
        if not m:
            continue
        case = m.group(1).replace("audit-", "")
        for ln in block:
            mm = STACK.match(ln) or OP_LINE.match(ln) or CONT.match(ln)
            if mm and mm.group(mm.lastindex) in nameset:
                found[case].add(mm.group(mm.lastindex))
    return found


def main():
    args = sys.argv[1:]
    fix = "--fix" in args
    args = [a for a in args if a != "--fix"]
    if not args:
        raise SystemExit(__doc__)
    text = Path(args[0]).read_text()
    names = next(
        (ln.split()[1:] for ln in text.splitlines() if ln.startswith("OPLIST ")),
        None,
    )
    if not names:
        raise SystemExit("no OPLIST line in the dump; re-run bytecode_dump_audit")
    nameset = set(names)
    order = {n: i for i, n in enumerate(names)}

    claims = {}
    for ln in text.splitlines():
        m = CLAIM.match(ln)
        if m:
            claims[m.group(1)] = set(m.group(2).split())

    found = found_in_dump(text, nameset)
    missing = []
    for case, ops in sorted(claims.items()):
        for op in sorted(ops):
            if op not in found.get(case, ()):
                others = sorted(c for c in found if op in found[c] and c != case)
                missing.append((op, case, others))

    if missing:
        print(f"{len(missing)} claim(s) not present in that case's bytecode:")
        for op, case, others in missing:
            elsewhere = f"  (present in: {', '.join(others)})" if others else "  (nowhere)"
            print(f"  {case} -> {op}{elsewhere}")
    else:
        print(f"ok: {sum(len(v) for v in claims.values())} claims verified against {len(found)} cases")

    if fix and missing:
        src = TESTS.read_text()

        def replace(m):
            indent, case = m.group(1), m.group(2)
            ops = sorted(found.get(case, ()), key=lambda o: order[o])
            return f"{indent}{case} => [{', '.join(ops)}]"

        src = re.sub(r"^(\s{4})(\w+) => \[[^\]]*\]$", replace, src, flags=re.M)
        TESTS.write_text(src)
        print("rewrote the claim lists in tests/jit/jit_opcodes.rs")

    sys.exit(1 if missing else 0)


if __name__ == "__main__":
    main()
