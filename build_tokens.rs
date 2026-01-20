use std::path::Path;

static TOKENS: &[&str] = &[
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
    //  KEYWORDS BEGIN - DON'T CHANGE ORDER (same order as ATOMS)
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
    "AWAIT", // must be last keyword
    "OF",    // only used for js_parse_skip_parens_token()
];

// const TOKEN_START_ID: i32 = -128;
const TOKEN_START_KEYWORD: &str = "NULL";
const TOKEN_LAST_KEYWORD: &str = "AWAIT";

pub fn gen_tokens(outfile: &Path) {
    let mut output = Vec::<String>::with_capacity(1024);

    // token enum
    output.push(
        "#[repr(u8)]\n#[derive(Debug,Clone,Copy,PartialEq,PartialOrd)]\npub(super) enum TokenKind {"
            .into(),
    );
    for tok in TOKENS.iter() {
        output.push(format!("{tok},"));
    }
    output.push("Char(char),".to_string());
    output.push("}\n".into());

    let kw_begin = TOKENS
        .iter()
        .position(|s| s.eq(&TOKEN_START_KEYWORD))
        .unwrap();
    let kw_end: usize = TOKENS
        .iter()
        .position(|s| s.eq(&TOKEN_LAST_KEYWORD))
        .unwrap();

    output.push(
        r#"impl Default for TokenKind {
            fn default() -> Self {
                Self::Char('\0')
            }
        }
        "#
        .into(),
    );

    output.push("impl TokenKind {\n".into());

    output.push("pub const KEYWORD_TOKEN_RANGE: std::ops::RangeInclusive<TokenKind> = Self::NULL..=Self::AWAIT;".into());
    output.push(
        r#"#[inline(always)]
        pub fn is_keyword(&self) -> bool {{ Self::KEYWORD_TOKEN_RANGE.contains(self) }}
        "#
        .into(),
    );

    // keyword to string
    output.push("pub const fn keyword_str(&self)->Option<&'static str> { match self {".into());
    for t in &TOKENS[kw_begin..=kw_end] {
        output.push(format!("Self::{t} => Some(\"{}\"),", t.to_lowercase()));
    }
    output.push("_=>None}".into());
    output.push("}\n".into());

    output.push("pub const fn keyword_atom(&self)->Option<crate::Atom>{ match self {".into());
    for (i, t) in TOKENS[kw_begin..=kw_end].iter().enumerate() {
        output.push(format!(
            "Self::{t} => Some(crate::Atom::new_const({})),",
            i + 1
        ));
    }
    output.push("_=>None}".into());
    output.push("}\n".into());

    output
        .push("pub fn from_keyword_atom(atom:crate::Atom)->Option<Self>{ match atom.id() {".into());
    for (i, t) in TOKENS[kw_begin..=kw_end].iter().enumerate() {
        output.push(format!("{} => Some(Self::{t}),", i + 1));
    }
    output.push("_=>None}".into());
    output.push("}\n".into());

    output.push("}\n".into()); // end of impl

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
