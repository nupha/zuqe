#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]
#![warn(clippy::missing_const_for_fn)]

pub mod array;
pub mod array_buffer;
pub mod atom;
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
pub mod js_core;
pub mod json;
pub mod map;
pub mod mem;
pub mod module;
pub mod number;
pub mod object;
pub mod parse;
pub mod promise;
pub mod proxy;
pub mod reflect;
pub mod regexp;
pub mod runtime;
pub mod stack_frame;
pub mod string;
pub mod symbol;
pub mod typed_array;
pub mod unicode;
pub mod utils;
pub mod value;

#[deprecated(note = "unused stuff")]
pub mod obsoletes;

include!(concat!(env!("OUT_DIR"), "/std_class_def.rs"));
