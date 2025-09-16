use crate::{iterator::JSIteratorKindEnum, value::JSValue};

use super::MapPair;

#[derive(Debug)]
pub struct JSMapIteratorData {
    pub obj: JSValue,
    pub kind: JSIteratorKindEnum,
    pub(crate) cur_record: MapPair,
}
