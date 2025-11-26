#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]
#![warn(clippy::missing_const_for_fn)]

pub mod atom;
pub mod builtins;
pub mod bytecode;
pub mod callable;
pub mod class;
pub mod context;
pub mod date;
pub mod error;
pub mod eval;
pub mod function;
pub mod gc;
pub mod generator;
pub mod global;
pub mod iterator;
pub mod json;
pub mod mapset;
pub mod mem;
pub mod module;
pub mod number;
pub mod object;
pub mod parse;
pub mod reflect;
pub mod runtime;
pub mod stack_frame;
pub mod string;
pub mod symbol;
pub mod utils;
pub mod value;

pub(crate) mod branch_hints;
pub(crate) mod resource;

include!(concat!(env!("OUT_DIR"), "/std_class_def.rs"));
