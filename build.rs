mod build_atoms;
mod build_opcodes;
mod build_stdclass;
mod build_tokens;

use std::{env, path::PathBuf};

use build_atoms::gen_atoms;
use build_opcodes::gen_opcodes;
use build_stdclass::gen_std_class_defs;
use build_tokens::gen_tokens;

fn build_c_alloca() {
    let mut builder = cc::Build::new();

    #[cfg(debug_assertions)]
    builder.flag_if_supported("-fstack-clash-protection");

    (if option_env!("CC") == Some("clang") {
        builder.flag("-flto")
    } else {
        &mut builder
    })
    .file("alloca.c")
    .opt_level(2)
    .compile("alloca");
}

fn main() {
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    build_c_alloca();

    gen_atoms(&out_path.join("atoms.rs"));
    gen_opcodes(&out_path.join("opcodes.rs"));
    gen_tokens(&out_path.join("tokens.rs"));
    gen_std_class_defs(&out_path.join("js_std_class_def.rs"));
}
