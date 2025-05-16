mod afun;
mod argument;
mod bound;
mod bytecode;
mod call;
mod clousure;
mod cpool;
mod fdef;
mod fun;
mod label;
mod native;
mod optimize;
mod scope;
mod stack;
mod var;
mod var_ref;
mod var_resolve;

pub use {
    afun::*, argument::*, bound::*, bytecode::*, call::*, clousure::*, fdef::*, fun::*, label::*,
    native::*, optimize::*, scope::*, stack::*, var::*, var_ref::*, var_resolve::*,
};
