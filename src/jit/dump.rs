// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Cranelift IR + machine-code dump (feature `jit_dump`, **off by default**).
//!
//! The whole path — IR formatting, file naming, writing — sits behind
//! `#[cfg(feature = "jit_dump")]`, so a build without the feature contains none
//! of it (`tests/jit/ci-gate.sh` greps the release artifact for `.clif` to keep
//! the `#[cfg]` honest). `JIT_DUMP` is read **once**, at attach
//! (`JitState::new`); `JIT_DUMP_DIR` (default `.`) selects the directory, and
//! the first attach of a process wins. Without the feature the variable is
//! never read: setting it is a silent no-op.
//!
//! Files are `<function-id>-<tier>.clif` (the Cranelift IR, prefixed with a
//! header line carrying the code size) and `<function-id>-<tier>.bin` (the raw
//! machine code). No disassembler: the dump exists to read IR and count bytes.

use std::{fmt::Write as _, path::PathBuf, sync::OnceLock};

use cranelift_codegen::ir::Function;

use super::runtime::Tier;

/// The dump directory; `None` until an attach turns dumping on.
static DEST: OnceLock<PathBuf> = OnceLock::new();

/// Read `JIT_DUMP` (and `JIT_DUMP_DIR`) once, when the JIT attaches. `0` and
/// the empty value keep the dump off.
pub(crate) fn read_env() {
    if DEST.get().is_some() {
        return;
    }
    let Some(value) = std::env::var_os("JIT_DUMP") else {
        return;
    };
    if value.is_empty() || value == "0" {
        return;
    }
    let dir = std::env::var_os("JIT_DUMP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let _ = DEST.set(dir);
}

/// Test hook: dump into `dir` for this process. Setting env vars from a test
/// would race with the other tests' attaches, so the smoke test drives the
/// same state directly.
#[cfg(test)]
pub(crate) fn enable_for_test(dir: PathBuf) {
    let _ = DEST.set(dir);
}

/// One function's dump. Constructed *before* codegen (which may rewrite the
/// IR), written after it succeeded; `None` when the dump is off, so the caller
/// formats nothing.
pub(crate) struct Dump(Option<Prepared>);

struct Prepared {
    dir: PathBuf,
    stem: String,
    ir: String,
}

impl Dump {
    /// Start a dump of `func` under `<function_id>-<tier>` unless `JIT_DUMP`
    /// was off. `Function: Display` writes clif.
    pub(crate) fn begin(function_id: u64, tier: Tier, func: &Function) -> Self {
        let Some(dir) = DEST.get() else {
            return Self(None);
        };
        let stem = format!("{function_id}-{}", tier_label(tier));
        let mut ir = String::new();
        let _ = write!(ir, "{func}");
        Self(Some(Prepared {
            dir: dir.clone(),
            stem,
            ir,
        }))
    }

    /// Write `<stem>.clif` and `<stem>.bin`. Failures are ignored: a
    /// diagnostic must never fail a compile (the worker path must not panic).
    pub(crate) fn finish(self, code: &[u8]) {
        let Some(prepared) = self.0 else {
            return;
        };
        let _ = std::fs::create_dir_all(&prepared.dir);
        let header = format!(
            "; zuqe jit_dump: {} code-bytes={}\n",
            prepared.stem,
            code.len()
        );
        let _ = std::fs::write(
            prepared.dir.join(format!("{}.clif", prepared.stem)),
            format!("{header}{}", prepared.ir),
        );
        let _ = std::fs::write(prepared.dir.join(format!("{}.bin", prepared.stem)), code);
    }
}

const fn tier_label(tier: Tier) -> &'static str {
    match tier {
        Tier::Cold => "cold",
        Tier::Tier1 => "tier1",
        Tier::Tier2 => "tier2",
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::jit::test_harness::settle;
    use crate::{EvalFlags, EvalName, EvalType, Jit, JitConfig, Runtime, ScopedContext};

    /// The dumps in `dir`, as `(stem, clif text)`, or the stems that are still
    /// missing their `.bin` or the header line. The destination is
    /// process-global, so the other tests' runtimes dump here too: `finish`
    /// writes the `.clif` before the `.bin` and each write truncates first, so
    /// a half-written pair is a transient state, not a failure.
    fn dumps(dir: &std::path::Path) -> Result<Vec<(String, String)>, Vec<String>> {
        let mut complete = Vec::new();
        let mut pending = Vec::new();
        for entry in std::fs::read_dir(dir).expect("read dump dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_none_or(|e| e != "clif") {
                continue;
            }
            let stem = path
                .file_stem()
                .expect("stem")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.contains("code-bytes=") && path.with_extension("bin").is_file() {
                complete.push((stem, text));
            } else {
                pending.push(stem);
            }
        }
        if pending.is_empty() {
            Ok(complete)
        } else {
            Err(pending)
        }
    }

    /// Feature-gate smoke: one hot function through the real pipeline must
    /// leave a `.clif`/`.bin` pair in the dump directory (run with
    /// `cargo test --features jit_dump --lib jit::dump`; the default feature
    /// set compiles none of this).
    #[test]
    fn a_compiled_function_dumps_its_ir_and_code() {
        let dir = std::env::temp_dir().join(format!("zuqe-jit-dump-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dump dir");
        enable_for_test(dir.clone());

        let mut rt = Runtime::create().expect("runtime");
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .expect("context");
        let jit = Jit::attach_to(
            &rt,
            JitConfig::builder()
                .call_threshold(1)
                .loop_threshold(1)
                .build(),
        )
        .expect("attach");
        ctx.eval(
            b"function f(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } f(64);",
            &EvalName::Name("jit-dump-smoke.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .expect("eval");
        settle(&jit);
        assert!(
            jit.metrics().functions_compiled > 0,
            "nothing compiled, nothing to dump"
        );

        // Wait until the directory holds only complete pairs: a foreign
        // writer may still be between the `.clif` and the `.bin`.
        let deadline = Instant::now() + Duration::from_secs(5);
        let dumps = loop {
            match dumps(&dir) {
                Ok(dumps) if !dumps.is_empty() => break dumps,
                other => {
                    assert!(
                        Instant::now() < deadline,
                        "{dir:?} never held a complete dump: {other:?}"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        };
        // Every IR dump has its machine code next to it, under the same
        // `<function-id>-<tier>` stem.
        for (stem, _) in &dumps {
            assert!(
                stem.contains('-'),
                "{stem}: the dump is named <function-id>-<tier>"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
