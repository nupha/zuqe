use std::path::Path;

use once_cell::sync::Lazy;

static TOKENS: Lazy<Vec<&str>> = Lazy::new(|| {
    let tokens = vec![
        "NUMBER",
        "STRING",
        "TEMPLATE",
        "IDENT",
        "REGEXP",
        "MUL_ASSIGN",
        "DIV_ASSIGN",
        "MOD_ASSIGN",
        "PLUS_ASSIGN",
        "MINUS_ASSIGN",
        "SHL_ASSIGN",
        "SAR_ASSIGN",
        "SHR_ASSIGN",
        "AND_ASSIGN",
        "XOR_ASSIGN",
        "OR_ASSIGN",
        "POW_ASSIGN",
        "LAND_ASSIGN",
        "LOR_ASSIGN",
        "DOUBLE_QUESTION_MARK_ASSIGN",
        "DEC",
        "INC",
        "SHL",
        "SAR",
        "SHR",
        "LT",
        "LTE",
        "GT",
        "GTE",
        "EQ",
        "STRICT_EQ",
        "NEQ",
        "STRICT_NEQ",
        "LAND",
        "LOR",
        "POW",
        "ARROW",
        "ELLIPSIS",
        "DOUBLE_QUESTION_MARK",
        "QUESTION_MARK_DOT",
        "ERROR",
        "PRIVATE_NAME",
        "EOF",
        //
        //  keywords: WARNING: same order as atoms
        //
        "NULL", // must be first keyword
        "FALSE",
        "TRUE",
        "IF",
        "ELSE",
        "RETURN",
        "VAR",
        "THIS",
        "DELETE",
        "VOID",
        "TYPEOF",
        "NEW",
        "IN",
        "INSTANCEOF",
        "DO",
        "WHILE",
        "FOR",
        "BREAK",
        "CONTINUE",
        "SWITCH",
        "CASE",
        "DEFAULT",
        "THROW",
        "TRY",
        "CATCH",
        "FINALLY",
        "FUNCTION",
        "DEBUGGER",
        "WITH",
        "CLASS",
        "CONST",
        "ENUM",
        "EXPORT",
        "EXTENDS",
        "IMPORT",
        "SUPER",
        "IMPLEMENTS",
        "INTERFACE",
        "LET",
        "PACKAGE",
        "PRIVATE",
        "PROTECTED",
        "PUBLIC",
        "STATIC",
        "YIELD",
        "AWAIT", // must be last
        "OF",    //only used for js_parse_skip_parens_token()
    ];
    tokens
});

const TOKEN_START_ID: i32 = -128;

pub fn gen_tokens(outfile: &Path) {
    let mut output = Vec::<String>::with_capacity(1024);

    for (i, tok) in TOKENS.iter().enumerate() {
        output.push(format!(
            "pub const TOK_{tok}: i32 = {};",
            i as i32 + TOKEN_START_ID
        ));
    }

    output.push("".into());
    std::fs::write(outfile, output.join("\n")).unwrap();

    let o = std::process::Command::new("rustfmt")
        .arg("--")
        .arg(outfile)
        .output()
        .expect("Failed to execute rustfmt");
    if !o.status.success() {
        eprintln!("rustfmt failed: {}", String::from_utf8_lossy(&o.stderr));
        std::process::exit(1);
    }
}
