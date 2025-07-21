use crate::atom::JSAtom;

#[derive(Debug)]
pub struct JSImportEntry {
    pub var_idx: u16, // closure variable index
    pub import_name: JSAtom,
    pub req_module_idx: i32, // in req_module_entries
}
