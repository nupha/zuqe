# test262 runner

Runs [test262](https://github.com/tc39/test262) against the zuqe engine,
filtered by ECMAScript edition presets (`ES5` … `ES2027`) and by spec suite
(ECMA-262 vs ECMA-402).

## Build & run

`intl402` is not a default feature, and the runner declares it via
`required-features` — builds without it fail with a clear error. Every
build/run must therefore pass `--features intl402`. Outward conformance runs
use release builds:

```sh
cargo build --release --example test262 --features intl402
cargo run --release --example test262 --features intl402 -- [ARGS]  # `--` separates cargo flags from runner flags
```

Release uses `panic = "abort"`, so a mid-run crash gives no panic output —
pair release runs with `--log-file` (see CLI) to identify the crashing
script.

Debug builds keep engine assertions and GC checks on, which surfaces bugs
locally:

```sh
cargo run --example test262 --features intl402 -- --preset ES2025
```

## CLI

| Flag | Meaning |
|---|---|
| `--config <FILE>` | Config file. Default: `test262.yml` in CWD. |
| `--preset <NAME>` | Edition preset to run (suites `262`/`all`). Falls back to the config's `preset:`. Append `!` for strict mode (see below). |
| `--suite <SUITE>` | Which spec's tests to collect: `262` (ECMA-262 only, **default**), `402` (`intl402/**` only), `all`. |
| `--features <LIST>` | Comma-separated feature whitelist. Runs only tests declaring one of them; reports as preset `USER`. Overrides `exclude_features` for the listed names. Not needed for suite `402`. |
| `--exclude-feature <F>` | Skip tests requiring this feature. Repeatable. Merged into the config's `exclude_features:`. |
| `--intl <LIST>` | Allowed ECMA-402 features for suite `402` (comma-separated whitelist). Overrides the config's `intl:`. |
| `[TEST_FILES]...` | Test files or globs, relative to `<base_path>/test/`. Replaces the config's `tests:` and bypasses `--suite` shaping. |
| `--error-file <FILE>` | Re-run only the failures listed in this JSONL file, then rewrite it in place (previous copy saved as `<FILE>-last.jsonl`). All other filtering is ignored. Default when absent: `test262-errors.jsonl` receives current failures. |
| `--log-file <FILE>` | Writes each script path (flushed) before it runs; after a crash the last line is the culprit. Required for release runs (`panic = "abort"` — the panic hook never fires there); in debug/unwind builds the hook covers Rust panics, but a hard crash (SEGV) still bypasses it. |

### Filtering semantics (exact)

- A test is **skipped** if any of its declared features was introduced in a
  strictly newer edition than the selected preset, or is listed in
  `exclude_features`.
- Features not named in any preset (stage-3 proposals, ECMA-402 flags) count as
  "newest edition": they run only under the latest preset — except under suite
  `402`, where they are ruled by the `intl:` whitelist instead.
- Suite `402` whitelist rule: a test is skipped when it declares any
  ECMA-402/proposal feature that is not listed in `intl:` (`--intl` overrides).
  Untagged baseline tests always run. Absent/empty `intl:` = unrestricted.
- Tests with no `features:` frontmatter always run in non-strict preset mode
  (and in suite `402`); strict mode (`NAME!`) skips them.
- Strict mode (`--preset NAME!`): runs only tests having at least one feature
  newly introduced in exactly that edition.
- USER mode (`--features`): runs only tests declaring at least one whitelisted
  feature; tests without features are skipped; edition gating is off.

## Config file

See `test262.sample.yml` for a full annotated template.

```yaml
base_path: "/path/to/test262"   # checkout containing test/ and harness/
preset: ES2026                  # selected preset; must name an entry below
suite: all                      # all | 262 | 402 (default 262)
tests: []                       # whitelist globs, relative to base_path/test; empty = all
exclude_tests: ["annexB/**/*.js", "staging/**/*.js"]
presets:                        # ordered; each entry inherits all earlier ones
  - ES5: ["IsHTMLDDA", "host-gc-required"]
  - ES2015: ["let", "const", "class", "Symbol", "Proxy", "Promise"]
  # ...
  - ES2026: ["Error.isError", "upsert"]
features: []                    # non-empty => USER mode
exclude_features: ["Temporal"]  # always skipped, every suite/preset
intl: ["Intl.Segmenter"]        # allowed ECMA-402 features for suite 402
                                # (absent/empty = unrestricted)
```

Rules:

- `presets:` order matters; later entries may rely on engine support from
  earlier ones. Each name must be unique.
- Every string in a preset must be an official test262 feature flag
  (<https://github.com/tc39/test262/blob/main/features.txt>).
- Edition assignment of stage-4 proposals follows
  <https://github.com/tc39/proposals/blob/main/finished-proposals.md>
  ("Expected Publication Year").
- ECMA-402 (`Intl.*`, `canonical-tz`) does not belong to any ECMA-262 edition
  and is never gated by presets: run it via `--suite 402` and select allowed
  flags with `intl:` / `--intl` (`exclude_features` still blocks globally,
  every suite).

## Suites

| Suite | Collects | Typical use |
|---|---|---|
| `262` (default) | everything except `intl402/**`, `staging/intl402/**` | language conformance |
| `402` | `intl402/**` only | Intl conformance |
| `all` | everything | full sweep |

An explicit whitelist (positional args or `tests:`) replaces collection
entirely; suite then has no effect on which files are gathered.

### Preset vs suite

- Suites `262` and `all` run the ECMA-262 conformance sweep: this is where the
  `preset:` ladder applies. It decides which edition's tests run.
- Suite `402` runs ECMA-402 and is gated by the `intl:` whitelist (config) or
  `--intl` (CLI): tests tagged only with non-whitelisted `Intl.*` features are
  skipped; untagged baseline tests always run; the preset plays no role.
  Unimplemented `Intl.*` flags can alternatively be blocked globally through
  `exclude_features:`.
- `Intl.*` flags themselves have no edition: under suite `402` the edition
  gate is pinned open, so the preset value does not affect which intl402 tests
  run.

## Recipes

```sh
# Full ECMA-262 run at the config's preset
cargo run --release --example test262 --features intl402 --

# Latest preset, everything including intl402
cargo run --release --example test262 --features intl402 -- --preset ES2027 --suite all

# Only intl402
cargo run --release --example test262 --features intl402 -- --suite 402

# One directory or file
cargo run --release --example test262 --features intl402 -- -- built-ins/Array/**

# Check one feature across suites
cargo run --release --example test262 --features intl402 -- --features iterator-helpers

# Quick-check a single Intl feature (suite 402)
cargo run --release --example test262 --features intl402 -- --suite 402 --intl Intl.Segmenter

# Iterate on failures: fix, re-run just those, repeat
cargo run --release --example test262 --features intl402 -- --error-file test262-errors.jsonl

# Release run with crash triage (last line of the log = crashing script)
cargo run --release --example test262 --features intl402 -- --log-file run.log
```

Note: the runner's own `--features` flag (test262 feature whitelist) is
unrelated to cargo's `--features`; keep cargo's before `--` and the runner's
after.

## Outputs

| File | Content |
|---|---|
| `test262-errors.jsonl` | One JSON object per failing script (`script`, error, features). Rewritten every run. |
| `<error-file>-last.jsonl` | Previous failure list (kept when `--error-file` re-runs). |
| `test262-report.html` | Run summary: totals, per-feature pass table, failed scripts grouped by feature. |

## Notes

- Test scripts and harness are loaded from `base_path`; point it at a real
  test262 checkout (git clone is enough).
- `preset: ES20XX!` strict mode is useful to see exactly what one edition adds.
- The runner executes both strict and non-strict variants of each test as
  required by test262's INTERPRETING.md; no extra configuration is needed.
