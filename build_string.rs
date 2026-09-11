// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

pub fn gen_strings(outfile: &Path) {
    let mut text = Vec::<String>::with_capacity(256);

    // generate string for commonly used ascii char.
    // and make ascii char to string LUT
    output.push(format!(
        "/// lut for ascii char 0-0x79\npub const JS_ATOM_ASCII_LUT: [AtomRef;{}] = [",
        0x80
    ));
    for c in 0..0x80 {
        text.push(format!("Rc::new(StringBuf::Ascii(\\x{c:02x}"));
        output.push(format!("AtomRef({atom_id}), // {c}"));
        atom_id += 1;
    }
    output.push("];".into());

    // full atom text data
    let full_cstr_buf = text.iter().map(|t| format!("{t}\\0")).collect::<String>();
}
