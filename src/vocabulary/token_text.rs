//! A piece of Rust syntax as text on one line, spaced as rustfmt spaces it, for the types and
//! signatures the vocabulary lists.

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens;

/// The keywords that stand apart from a delimiter after them, as in `&mut [u8]` and `dyn (A)`,
/// where any other word joins it, as in `Fn(u8)`, `pub(crate)` and `name!(…)`.
const KEYWORDS: [&str; 12] = [
    "as", "const", "dyn", "extern", "impl", "in", "move", "mut", "ref", "static", "unsafe", "where",
];

/// The text of `tokens`.
pub(crate) fn of(tokens: &impl ToTokens) -> String {
    let mut pieces = Vec::new();
    flatten(tokens.to_token_stream(), &mut pieces);
    render(&without_trailing_commas(pieces))
}

/// The text of each of `items`, `separator` between them, so a trailing separator the source
/// holds is left out.
pub(crate) fn joined<'a, T: ToTokens + 'a>(
    items: impl IntoIterator<Item = &'a T>,
    separator: &str,
) -> String {
    let texts: Vec<String> = items.into_iter().map(of).collect();
    texts.join(separator)
}

/// A token as the spacing reads it: a word, or an operator of its joined characters.
#[derive(Debug, PartialEq, Eq)]
enum Piece {
    /// An identifier other than `KEYWORDS`, or a literal.
    Word(String),
    Keyword(String),
    Lifetime(String),
    Op(String),
    Open(Delimiter),
    Close(Delimiter),
}

/// What a piece is to the pieces beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Word,
    Keyword,
    Lifetime,
    /// `,`, `;` and `:`, which join the piece before them.
    Separator,
    /// `::`, `.` and the ranges, which join both sides.
    Tight,
    AngleOpen,
    AngleClose,
    /// An operator that joins the piece after it, as `&`, `*`, `?`, `#` and a unary `-` or `!`.
    Prefix,
    /// The `!` of a macro's call, which joins both sides.
    Bang,
    /// An operator that stands apart from both sides.
    Infix,
    Open(Delimiter),
    Close(Delimiter),
}

fn flatten(stream: TokenStream, pieces: &mut Vec<Piece>) {
    let mut op = String::new();
    let mut tick = false;
    for tree in stream {
        // A lifetime's tick is part of no operator, joined to one or not.
        let ends_op = match &tree {
            TokenTree::Punct(punct) => punct.as_char() == '\'',
            _ => true,
        };
        if ends_op && !op.is_empty() {
            pieces.push(Piece::Op(std::mem::take(&mut op)));
        }
        match tree {
            TokenTree::Punct(punct) if punct.as_char() == '\'' => tick = true,
            TokenTree::Punct(punct) => {
                op.push(punct.as_char());
                if punct.spacing() == Spacing::Alone {
                    pieces.push(Piece::Op(std::mem::take(&mut op)));
                }
            }
            TokenTree::Ident(ident) => {
                let text = ident.to_string();
                pieces.push(if std::mem::take(&mut tick) {
                    Piece::Lifetime(format!("'{text}"))
                } else if KEYWORDS.contains(&text.as_str()) {
                    Piece::Keyword(text)
                } else {
                    Piece::Word(text)
                });
            }
            TokenTree::Literal(literal) => pieces.push(Piece::Word(literal.to_string())),
            TokenTree::Group(group) if group.delimiter() == Delimiter::None => {
                flatten(group.stream(), pieces);
            }
            TokenTree::Group(group) => {
                pieces.push(Piece::Open(group.delimiter()));
                flatten(group.stream(), pieces);
                pieces.push(Piece::Close(group.delimiter()));
            }
        }
    }
    debug_assert!(!tick, "a lifetime's tick comes before its name");
    if !op.is_empty() {
        pieces.push(Piece::Op(op));
    }
}

/// A list a delimiter or angle brackets hold, by what a trailing comma means in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum List {
    /// What no delimiter holds.
    Top,
    /// Parentheses, where a trailing comma after one element makes a tuple.
    Parentheses,
    Angles,
    /// Brackets or braces.
    Other,
}

/// `pieces` without each comma that ends a list, which rustfmt leaves out of a list on one line:
/// all but the comma of a tuple of one element, `(A,)`.
fn without_trailing_commas(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut lists = vec![(List::Top, 0_usize)];
    let mut kept: Vec<Piece> = Vec::with_capacity(pieces.len());
    for piece in pieces {
        let closes = match &piece {
            Piece::Open(delimiter) => {
                let list = if *delimiter == Delimiter::Parenthesis {
                    List::Parentheses
                } else {
                    List::Other
                };
                lists.push((list, 0));
                None
            }
            Piece::Op(op) if op == "<" => {
                lists.push((List::Angles, 0));
                None
            }
            Piece::Op(op) if op == "," => {
                lists.last_mut().expect("the top list stays").1 += 1;
                None
            }
            Piece::Close(_) => lists.pop(),
            // A `>` that closes no `<` compares.
            Piece::Op(op)
                if op == ">" && lists.last().is_some_and(|&(list, _)| list == List::Angles) =>
            {
                lists.pop()
            }
            _ => None,
        };
        if let Some((list, commas)) = closes {
            let tuple_of_one = list == List::Parentheses && commas == 1;
            if !tuple_of_one && matches!(kept.last(), Some(Piece::Op(op)) if op == ",") {
                kept.pop();
            }
        }
        kept.push(piece);
    }
    kept
}

fn render(pieces: &[Piece]) -> String {
    let mut text = String::new();
    let mut before: Option<Role> = None;
    for (at, piece) in pieces.iter().enumerate() {
        let role = role(piece, before, pieces.get(at + 1));
        if before.is_some_and(|before| spaced(before, role)) {
            text.push(' ');
        }
        match piece {
            Piece::Word(word) | Piece::Keyword(word) | Piece::Lifetime(word) | Piece::Op(word) => {
                text.push_str(word);
            }
            Piece::Open(delimiter) => text.push_str(match delimiter {
                Delimiter::Parenthesis => "(",
                Delimiter::Brace => "{",
                Delimiter::Bracket => "[",
                Delimiter::None => unreachable!("an invisible group is flattened"),
            }),
            Piece::Close(delimiter) => text.push_str(match delimiter {
                Delimiter::Parenthesis => ")",
                Delimiter::Brace => "}",
                Delimiter::Bracket => "]",
                Delimiter::None => unreachable!("an invisible group is flattened"),
            }),
        }
        before = Some(role);
    }
    text
}

/// The role of `piece`, after a piece of role `before` and before `after`: an operator that
/// can be unary or binary is unary unless an operand ends before it.
fn role(piece: &Piece, before: Option<Role>, after: Option<&Piece>) -> Role {
    let op = match piece {
        Piece::Word(_) => return Role::Word,
        Piece::Keyword(_) => return Role::Keyword,
        Piece::Lifetime(_) => return Role::Lifetime,
        Piece::Open(delimiter) => return Role::Open(*delimiter),
        Piece::Close(delimiter) => return Role::Close(*delimiter),
        Piece::Op(op) => op.as_str(),
    };
    let operand_ends = matches!(
        before,
        Some(Role::Word | Role::Lifetime | Role::AngleClose | Role::Close(_))
    );
    match op {
        "," | ";" | ":" => Role::Separator,
        "::" | "." | ".." | "..=" => Role::Tight,
        "<" => Role::AngleOpen,
        ">" => Role::AngleClose,
        "!" if before == Some(Role::Word) && matches!(after, Some(Piece::Open(_))) => Role::Bang,
        "&" | "&&" | "*" | "-" | "!" if !operand_ends => Role::Prefix,
        "?" | "#" | "$" => Role::Prefix,
        _ => Role::Infix,
    }
}

/// Whether a space stands between a piece of role `before` and one of role `after`.
const fn spaced(before: Role, after: Role) -> bool {
    match (before, after) {
        (Role::Open(Delimiter::Brace), Role::Close(Delimiter::Brace)) => false,
        (Role::Open(Delimiter::Brace), _) | (_, Role::Close(Delimiter::Brace)) => true,
        (Role::Open(_) | Role::Prefix | Role::Tight | Role::AngleOpen | Role::Bang, _)
        | (_, Role::Close(_) | Role::Separator | Role::Tight | Role::AngleClose | Role::Bang)
        | (Role::Word | Role::Keyword, Role::AngleOpen)
        | (
            Role::Word | Role::AngleClose,
            Role::Open(Delimiter::Parenthesis | Delimiter::Bracket),
        ) => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use syn::{Expr, Generics, Signature, Type};

    use super::*;

    #[test]
    fn types_read_as_rustfmt_writes_them() {
        let cases = [
            "u8",
            "Vec<Vec<u8>>",
            "&'a str",
            "&'static mut [u8]",
            "&'a [u8]",
            "&mut [u8; 4]",
            "[(); 4]",
            "(A,)",
            "()",
            "Option<(A, B)>",
            "*const *mut T",
            "Box<dyn Fn(&mut World) -> bool + Send + 'static>",
            "impl Iterator<Item = &'_ str> + '_",
            "dyn for<'a> Fn(&'a u8) -> &'a u8",
            "<T as Trait>::Out",
            "&<T as Trait>::Out",
            "Vec::<u8>",
            "unsafe extern \"C\" fn(u8) -> !",
            "[u8; N * 2 - 1]",
            "[u8; { N + 1 }]",
            "name!(a, b)",
            "fmt::Result",
            // One element's trailing comma makes a tuple; any other trailing comma means nothing.
            "(Vec<(A, B)>,)",
        ];
        for case in cases {
            let ty: Type = syn::parse_str(case).unwrap();
            assert_eq!(of(&ty), case);
        }
    }

    #[test]
    fn signatures_and_generics_read_as_rustfmt_writes_them() {
        let signature: Signature =
            syn::parse_str("fn f<'a, T: ?Sized + Copy>(&'a self, mut x: T) -> Self").unwrap();
        assert_eq!(of(&signature.inputs), "&'a self, mut x: T");
        assert_eq!(
            joined(&signature.generics.params, ", "),
            "'a, T: ?Sized + Copy"
        );
        // A trailing comma the source holds is not joined.
        let generics: Generics = syn::parse_str("<A, B,>").unwrap();
        assert_eq!(of(&generics.params), "A, B,");
        assert_eq!(joined(&generics.params, ", "), "A, B");
        let ty: Type = syn::parse_str("(Query<'w, A, B,>, [u8; 2],)").unwrap();
        assert_eq!(of(&ty), "(Query<'w, A, B>, [u8; 2])");
        let pattern: Signature = syn::parse_str("fn f(Line { a, b, }: Line)").unwrap();
        assert_eq!(of(&pattern.inputs), "Line { a, b }: Line");
        let expr: Expr = syn::parse_str("-1").unwrap();
        assert_eq!(of(&expr), "-1");
        let expr: Expr = syn::parse_str("!a").unwrap();
        assert_eq!(of(&expr), "!a");
    }
}
