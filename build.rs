// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod build_atoms;
mod build_opcodes;
mod build_stdclass;
mod build_tokens;

use std::{env, path::PathBuf};

use build_atoms::gen_atoms;
use build_opcodes::gen_opcodes;
use build_stdclass::{gen_class_payload, gen_object_casts, gen_std_class_defs};
use build_tokens::gen_tokens;

fn main() {
    // Regenerate whenever the build-script sources change.
    println!("cargo:rerun-if-changed=build_atoms.rs");
    println!("cargo:rerun-if-changed=build_opcodes.rs");
    println!("cargo:rerun-if-changed=build_stdclass.rs");
    println!("cargo:rerun-if-changed=build_tokens.rs");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    gen_atoms(&out_path.join("atoms.rs"));
    gen_opcodes(&out_path.join("opcodes.rs"));
    gen_tokens(&out_path.join("tokens.rs"));
    gen_std_class_defs(&out_path.join("std_class_def.rs"));
    gen_class_payload(&out_path.join("class_payload.rs"));
    gen_object_casts(&out_path.join("object_casts.rs"));
}
