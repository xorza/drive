use std::fmt::{self, Write};

use proc_macro2::Ident;
use syn::{
    Attribute, Field, Fields, ForeignItem, Generics, ImplItem, Item, ItemEnum, ItemImpl, ItemTrait,
    ReturnType, Safety, Signature, StaticMutability, TraitItem, Visibility,
};

use crate::vocabulary::index::{Index, IndexSection, NameListing};
use crate::vocabulary::token_text;

/// The items of one Rust file as lines of the layout, each item's own one deeper, and the
/// entries of the index they declare.
#[derive(Debug)]
pub(crate) struct Outline<'a> {
    text: &'a mut String,
    index: &'a mut Index,
    /// The file's path, then `::` and each inline module the items are in.
    location: String,
}

impl<'a> Outline<'a> {
    pub(crate) const fn new(text: &'a mut String, index: &'a mut Index, path: String) -> Self {
        Self {
            text,
            index,
            location: path,
        }
    }

    pub(crate) fn items(&mut self, items: &[Item], depth: usize) {
        for item in items {
            self.item(item, depth);
        }
    }

    fn item(&mut self, item: &Item, depth: usize) {
        match item {
            Item::Const(item) => {
                let ty = token_text::of(&item.ty);
                self.line(
                    depth,
                    format_args!(
                        "{}{}const {}{}: {ty}",
                        gates(&item.attrs),
                        visibility(&item.vis),
                        item.ident,
                        generics(&item.generics)
                    ),
                );
                // `const _` names nothing: it holds a check the compiler runs.
                if item.ident != "_" {
                    self.index.add(
                        IndexSection::Values,
                        NameListing {
                            detail: format!(": {ty}"),
                            ..self.entry(&item.ident, "const")
                        },
                    );
                }
            }
            Item::Enum(item) => self.enumeration(item, depth),
            Item::Fn(item) => {
                let line = format!("{}{}", visibility(&item.vis), signature(&item.sig));
                self.line(depth, format_args!("{}{line}", gates(&item.attrs)));
                self.add_function(&item.attrs, &item.sig.ident, "fn");
            }
            Item::ForeignMod(item) => {
                let abi = token_text::of(&item.abi);
                self.line(depth, format_args!("{}{abi}", gates(&item.attrs)));
                for foreign in &item.items {
                    self.foreign(foreign, &abi, depth + 1);
                }
            }
            Item::Impl(item) => self.implementation(item, depth),
            Item::Macro(item) => match &item.ident {
                Some(ident) if item.mac.path.is_ident("macro_rules") => {
                    let line = format!("macro_rules! {ident}");
                    self.line(depth, format_args!("{}{line}", gates(&item.attrs)));
                    self.add_function(&item.attrs, ident, "macro_rules");
                }
                _ => self.line(
                    depth,
                    format_args!("{}{}!", gates(&item.attrs), token_text::of(&item.mac.path)),
                ),
            },
            Item::Mod(item) => {
                let Some((_, items)) = &item.content else {
                    return;
                };
                self.line(
                    depth,
                    format_args!(
                        "{}{}mod {}",
                        gates(&item.attrs),
                        visibility(&item.vis),
                        item.ident
                    ),
                );
                self.index.add_words(&item.ident.to_string());
                let outer = self.location.len();
                write!(self.location, "::{}", item.ident).expect("a String takes every write");
                self.items(items, depth + 1);
                self.location.truncate(outer);
            }
            Item::Static(item) => {
                let ty = token_text::of(&item.ty);
                let mutability = static_mutability(&item.mutability);
                self.line(
                    depth,
                    format_args!(
                        "{}{}static {mutability}{}: {ty}",
                        gates(&item.attrs),
                        visibility(&item.vis),
                        item.ident
                    ),
                );
                self.index.add(
                    IndexSection::Values,
                    NameListing {
                        detail: format!(": {ty}"),
                        ..self.entry(&item.ident, "static")
                    },
                );
            }
            Item::Struct(item) => {
                self.line(
                    depth,
                    format_args!(
                        "{}{}struct {}{}{}",
                        gates(&item.attrs),
                        visibility(&item.vis),
                        item.ident,
                        generics(&item.generics),
                        tuple(&item.fields)
                    ),
                );
                self.index
                    .add(IndexSection::Types, self.entry(&item.ident, "struct"));
                let owner = format!("struct {}", item.ident);
                self.fields(&item.fields, &owner, depth + 1);
            }
            Item::Trait(item) => self.declare_trait(item, depth),
            Item::TraitAlias(item) => {
                self.line(
                    depth,
                    format_args!(
                        "{}{}trait {}{} = {}",
                        gates(&item.attrs),
                        visibility(&item.vis),
                        item.ident,
                        generics(&item.generics),
                        token_text::joined(&item.bounds, " + ")
                    ),
                );
                self.index
                    .add(IndexSection::Types, self.entry(&item.ident, "trait"));
            }
            Item::Type(item) => {
                self.line(
                    depth,
                    format_args!(
                        "{}{}type {}{} = {}",
                        gates(&item.attrs),
                        visibility(&item.vis),
                        item.ident,
                        generics(&item.generics),
                        token_text::of(&item.ty)
                    ),
                );
                self.index
                    .add(IndexSection::Types, self.entry(&item.ident, "type"));
            }
            Item::Union(item) => {
                self.line(
                    depth,
                    format_args!(
                        "{}{}union {}{}",
                        gates(&item.attrs),
                        visibility(&item.vis),
                        item.ident,
                        generics(&item.generics)
                    ),
                );
                self.index
                    .add(IndexSection::Types, self.entry(&item.ident, "union"));
                let owner = format!("union {}", item.ident);
                for field in &item.fields.named {
                    self.field(field, &owner, depth + 1);
                }
            }
            Item::ExternCrate(_) | Item::Use(_) => {}
            Item::Verbatim(tokens) => {
                self.line(depth, format_args!("{} (unparsed)", token_text::of(tokens)));
            }
            _ => self.line(depth, format_args!("(an item syn does not model)")),
        }
    }

    fn enumeration(&mut self, item: &ItemEnum, depth: usize) {
        self.line(
            depth,
            format_args!(
                "{}{}enum {}{}",
                gates(&item.attrs),
                visibility(&item.vis),
                item.ident,
                generics(&item.generics)
            ),
        );
        self.index
            .add(IndexSection::Types, self.entry(&item.ident, "enum"));
        let owner = format!("enum {}", item.ident);
        for variant in &item.variants {
            let discriminant = variant
                .discriminant
                .as_ref()
                .map_or_else(String::new, |(_, expr)| {
                    format!(" = {}", token_text::of(expr))
                });
            self.line(
                depth + 1,
                format_args!(
                    "{}{}{}{discriminant}",
                    gates(&variant.attrs),
                    variant.ident,
                    tuple(&variant.fields)
                ),
            );
            self.index
                .add(IndexSection::Variants, self.entry(&variant.ident, &owner));
            let owner = format!("variant {}::{}", item.ident, variant.ident);
            self.fields(&variant.fields, &owner, depth + 2);
        }
    }

    /// The lines of `item` and the entries of an inherent impl: a trait impl's names are the
    /// trait's, which its own declaration lists.
    fn implementation(&mut self, item: &ItemImpl, depth: usize) {
        let own_names = item.trait_.is_none();
        let mut header = String::new();
        if item.unsafety.is_some() {
            header.push_str("unsafe ");
        }
        header.push_str("impl ");
        if let Some((path, _)) = &item.trait_ {
            if item.modifiers.polarity.is_some() {
                header.push('!');
            }
            write!(header, "{} for ", token_text::of(path)).expect("a String takes every write");
        }
        header.push_str(&token_text::of(&item.self_ty));
        self.line(depth, format_args!("{}{header}", gates(&item.attrs)));
        for member in &item.items {
            match member {
                ImplItem::Const(member) => {
                    let ty = token_text::of(&member.ty);
                    self.line(
                        depth + 1,
                        format_args!(
                            "{}{}const {}: {ty}",
                            gates(&member.attrs),
                            visibility(&member.vis),
                            member.ident
                        ),
                    );
                    if own_names {
                        self.index.add(
                            IndexSection::Values,
                            NameListing {
                                detail: format!(": {ty}"),
                                ..self.entry(&member.ident, &header)
                            },
                        );
                    }
                }
                ImplItem::Fn(member) => {
                    let line = format!("{}{}", visibility(&member.vis), signature(&member.sig));
                    self.line(depth + 1, format_args!("{}{line}", gates(&member.attrs)));
                    if own_names {
                        self.add_function(&member.attrs, &member.sig.ident, &header);
                    }
                }
                ImplItem::Type(member) => {
                    self.line(
                        depth + 1,
                        format_args!(
                            "{}{}type {}{} = {}",
                            gates(&member.attrs),
                            visibility(&member.vis),
                            member.ident,
                            generics(&member.generics),
                            token_text::of(&member.ty)
                        ),
                    );
                    if own_names {
                        self.index
                            .add(IndexSection::Types, self.entry(&member.ident, &header));
                    }
                }
                ImplItem::Macro(member) => self.line(
                    depth + 1,
                    format_args!(
                        "{}{}!",
                        gates(&member.attrs),
                        token_text::of(&member.mac.path)
                    ),
                ),
                ImplItem::Verbatim(tokens) => self.line(
                    depth + 1,
                    format_args!("{} (unparsed)", token_text::of(tokens)),
                ),
                _ => self.line(depth + 1, format_args!("(an item syn does not model)")),
            }
        }
    }

    fn declare_trait(&mut self, item: &ItemTrait, depth: usize) {
        let supertraits = if item.supertraits.is_empty() {
            String::new()
        } else {
            format!(": {}", token_text::joined(&item.supertraits, " + "))
        };
        self.line(
            depth,
            format_args!(
                "{}{}{}{}trait {}{}{supertraits}",
                gates(&item.attrs),
                visibility(&item.vis),
                if item.unsafety.is_some() {
                    "unsafe "
                } else {
                    ""
                },
                if item.modifiers.auto_token.is_some() {
                    "auto "
                } else {
                    ""
                },
                item.ident,
                generics(&item.generics)
            ),
        );
        self.index
            .add(IndexSection::Types, self.entry(&item.ident, "trait"));
        let owner = format!("trait {}", item.ident);
        for member in &item.items {
            match member {
                TraitItem::Const(member) => {
                    let ty = token_text::of(&member.ty);
                    self.line(
                        depth + 1,
                        format_args!("{}const {}: {ty}", gates(&member.attrs), member.ident),
                    );
                    self.index.add(
                        IndexSection::Values,
                        NameListing {
                            detail: format!(": {ty}"),
                            ..self.entry(&member.ident, &owner)
                        },
                    );
                }
                TraitItem::Fn(member) => {
                    let line = signature(&member.sig);
                    self.line(depth + 1, format_args!("{}{line}", gates(&member.attrs)));
                    self.add_function(&member.attrs, &member.sig.ident, &owner);
                }
                TraitItem::Type(member) => {
                    let bounds = if member.bounds.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", token_text::joined(&member.bounds, " + "))
                    };
                    self.line(
                        depth + 1,
                        format_args!(
                            "{}type {}{}{bounds}",
                            gates(&member.attrs),
                            member.ident,
                            generics(&member.generics)
                        ),
                    );
                    self.index
                        .add(IndexSection::Types, self.entry(&member.ident, &owner));
                }
                TraitItem::Macro(member) => self.line(
                    depth + 1,
                    format_args!(
                        "{}{}!",
                        gates(&member.attrs),
                        token_text::of(&member.mac.path)
                    ),
                ),
                TraitItem::Verbatim(tokens) => self.line(
                    depth + 1,
                    format_args!("{} (unparsed)", token_text::of(tokens)),
                ),
                _ => self.line(depth + 1, format_args!("(an item syn does not model)")),
            }
        }
    }

    fn foreign(&mut self, item: &ForeignItem, abi: &str, depth: usize) {
        match item {
            ForeignItem::Fn(item) => {
                let line = format!("{}{}", visibility(&item.vis), signature(&item.sig));
                self.line(depth, format_args!("{}{line}", gates(&item.attrs)));
                self.add_function(&item.attrs, &item.sig.ident, abi);
            }
            ForeignItem::Static(item) => {
                let ty = token_text::of(&item.ty);
                let mutability = static_mutability(&item.mutability);
                self.line(
                    depth,
                    format_args!(
                        "{}{}static {mutability}{}: {ty}",
                        gates(&item.attrs),
                        visibility(&item.vis),
                        item.ident
                    ),
                );
                self.index.add(
                    IndexSection::Values,
                    NameListing {
                        detail: format!(": {ty}"),
                        ..self.entry(&item.ident, abi)
                    },
                );
            }
            ForeignItem::Type(item) => {
                self.line(
                    depth,
                    format_args!(
                        "{}{}type {}{}",
                        gates(&item.attrs),
                        visibility(&item.vis),
                        item.ident,
                        generics(&item.generics)
                    ),
                );
                self.index
                    .add(IndexSection::Types, self.entry(&item.ident, abi));
            }
            ForeignItem::Macro(item) => self.line(
                depth,
                format_args!("{}{}!", gates(&item.attrs), token_text::of(&item.mac.path)),
            ),
            ForeignItem::Verbatim(tokens) => {
                self.line(depth, format_args!("{} (unparsed)", token_text::of(tokens)));
            }
            _ => self.line(depth, format_args!("(an item syn does not model)")),
        }
    }

    /// The entry of a function, which a test has none of.
    fn add_function(&mut self, attrs: &[Attribute], name: &Ident, owner: &str) {
        if !attrs.iter().any(is_test) {
            self.index
                .add(IndexSection::Functions, self.entry(name, owner));
        }
    }

    /// The lines and entries of named `fields`; tuple fields have their struct's line or their
    /// variant's.
    fn fields(&mut self, fields: &Fields, owner: &str, depth: usize) {
        if let Fields::Named(fields) = fields {
            for field in &fields.named {
                self.field(field, owner, depth);
            }
        }
    }

    fn field(&mut self, field: &Field, owner: &str, depth: usize) {
        let ident = field.ident.as_ref().expect("a named field has a name");
        let ty = token_text::of(&field.ty);
        self.line(
            depth,
            format_args!(
                "{}{}{ident}: {ty}",
                gates(&field.attrs),
                visibility(&field.vis)
            ),
        );
        self.index.add(
            IndexSection::Fields,
            NameListing {
                detail: format!(": {ty}"),
                ..self.entry(ident, owner)
            },
        );
    }

    fn line(&mut self, depth: usize, content: fmt::Arguments<'_>) {
        line(self.text, depth, content);
    }

    /// The entry of `name`, which `owner` declares here, with no detail.
    fn entry(&self, name: &Ident, owner: &str) -> NameListing {
        NameListing {
            name: name.to_string(),
            detail: String::new(),
            owner: owner.to_owned(),
            location: self.location.clone(),
        }
    }
}

/// Writes `line` to the layout `text`, two spaces in for each step of `depth`.
pub(crate) fn line(text: &mut String, depth: usize, line: fmt::Arguments<'_>) {
    writeln!(text, "{:indent$}{line}", "", indent = depth * 2).expect("a String takes every write");
}

/// The attributes that decide whether an item is built or run, each as its source writes it
/// and a space after it: each `cfg` and each test's.
fn gates(attrs: &[Attribute]) -> String {
    let mut text = String::new();
    for attr in attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg") || is_test(attr))
    {
        write!(text, "#[{}] ", token_text::of(&attr.meta)).expect("a String takes every write");
    }
    text
}

/// Whether `attr` makes its function a test: `#[test]`, or one of a path that ends in `test`.
fn is_test(attr: &Attribute) -> bool {
    attr.path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "test")
}

fn visibility(vis: &Visibility) -> String {
    match vis {
        Visibility::Inherited => String::new(),
        _ => format!("{} ", token_text::of(vis)),
    }
}

/// The generic parameters of `generics`, without its `where` clause.
fn generics(generics: &Generics) -> String {
    if generics.params.is_empty() {
        String::new()
    } else {
        format!("<{}>", token_text::joined(&generics.params, ", "))
    }
}

/// The tuple fields of a struct or a variant, as its line ends; nothing for named fields or none.
fn tuple(fields: &Fields) -> String {
    let Fields::Unnamed(fields) = fields else {
        return String::new();
    };
    let each: Vec<String> = fields
        .unnamed
        .iter()
        .map(|field| {
            format!(
                "{}{}{}",
                gates(&field.attrs),
                visibility(&field.vis),
                token_text::of(&field.ty)
            )
        })
        .collect();
    format!("({})", each.join(", "))
}

const fn static_mutability(mutability: &StaticMutability) -> &'static str {
    match mutability {
        StaticMutability::Mut(_) => "mut ",
        _ => "",
    }
}

/// A function's signature, without its `where` clause.
fn signature(sig: &Signature) -> String {
    let mut text = String::new();
    if sig.constness.is_some() {
        text.push_str("const ");
    }
    if sig.asyncness.is_some() {
        text.push_str("async ");
    }
    match sig.safety {
        Safety::Unsafe(_) => text.push_str("unsafe "),
        Safety::Safe(_) => text.push_str("safe "),
        Safety::Default => {}
    }
    if let Some(abi) = &sig.abi {
        write!(text, "{} ", token_text::of(abi)).expect("a String takes every write");
    }
    let mut inputs = token_text::joined(&sig.inputs, ", ");
    if let Some(variadic) = &sig.variadic {
        if !inputs.is_empty() {
            inputs.push_str(", ");
        }
        inputs.push_str(&token_text::of(variadic));
    }
    write!(
        text,
        "fn {}{}({inputs})",
        sig.ident,
        generics(&sig.generics)
    )
    .expect("a String takes every write");
    if let ReturnType::Type(_, ty) = &sig.output {
        write!(text, " -> {}", token_text::of(ty)).expect("a String takes every write");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(source: &str) -> (String, String) {
        let file = syn::parse_file(source).unwrap();
        let mut text = String::new();
        let mut index = Index::default();
        Outline::new(&mut text, &mut index, "a.rs".to_owned()).items(&file.items, 1);
        let mut entries = String::new();
        index.write(&mut entries);
        (text, entries)
    }

    #[test]
    fn each_item_is_a_line_and_what_it_holds_is_under_it() {
        let source = "
            use std::fmt;
            mod child;
            /// A unit.
            #[derive(Debug)]
            pub struct Unit<T: Copy> where T: Send {
                pub(crate) id: UnitId,
                #[cfg(test)]
                probe: Vec<&'static str>,
            }
            struct Meters(pub u32, u8);
            enum Mode { Idle = 1, Moving(Vec2), Attacking { target: Entity } }
            impl<T: Copy> fmt::Display for Unit<T> {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { Ok(()) }
            }
            impl Module for Unit<u8> {
                type Spec = ();
                const NAME: &'static str = UNIT;
                fn install(&self, app: &mut App) {}
            }
            impl Unit<u8> {
                pub const MAX: u8 = 3;
                pub(crate) const fn new(id: UnitId) -> Self where Self: Sized { todo!() }
            }
            pub trait Module: Send + Sync {
                type Spec: Default;
                const NAME: &'static str;
                fn install(&self, app: &mut App);
            }
            static mut COUNT: u32 = 0;
            const _: () = assert!(true);
            type Map<K> = BTreeMap<K, u32>;
            macro_rules! each { () => {}; }
            thread_local! { static X: u8 = 0; }
            fn free() { fn inner() {} }
            #[cfg(test)]
            mod tests {
                #[test]
                fn a_test_passes() {}
                fn helper() {}
            }
        ";
        let (text, entries) = outline(source);
        let expected = concat!(
            "  pub struct Unit<T: Copy>\n",
            "    pub(crate) id: UnitId\n",
            "    #[cfg(test)] probe: Vec<&'static str>\n",
            "  struct Meters(pub u32, u8)\n",
            "  enum Mode\n",
            "    Idle = 1\n",
            "    Moving(Vec2)\n",
            "    Attacking\n",
            "      target: Entity\n",
            "  impl fmt::Display for Unit<T>\n",
            "    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result\n",
            "  impl Module for Unit<u8>\n",
            "    type Spec = ()\n",
            "    const NAME: &'static str\n",
            "    fn install(&self, app: &mut App)\n",
            "  impl Unit<u8>\n",
            "    pub const MAX: u8\n",
            "    pub(crate) const fn new(id: UnitId) -> Self\n",
            "  pub trait Module: Send + Sync\n",
            "    type Spec: Default\n",
            "    const NAME: &'static str\n",
            "    fn install(&self, app: &mut App)\n",
            "  static mut COUNT: u32\n",
            "  const _: ()\n",
            "  type Map<K> = BTreeMap<K, u32>\n",
            "  macro_rules! each\n",
            "  thread_local!\n",
            "  fn free()\n",
            "  #[cfg(test)] mod tests\n",
            "    #[test] fn a_test_passes()\n",
            "    fn helper()\n",
        );
        assert_eq!(text, expected);
        // A test and a trait impl's items are in the layout alone, as the trait names the
        // latter; an inline module's items are at its path.
        let expected = concat!(
            "\n== Types (6) ==\n\n",
            "Map  type  a.rs\n",
            "Meters  struct  a.rs\n",
            "Mode  enum  a.rs\n",
            "Module  trait  a.rs\n",
            "Spec  trait Module  a.rs\n",
            "Unit  struct  a.rs\n",
            "\n== Functions (5) ==\n\n",
            "each  macro_rules  a.rs\n",
            "free  fn  a.rs\n",
            "helper  fn  a.rs::tests\n",
            "install  trait Module  a.rs\n",
            "new  impl Unit<u8>  a.rs\n",
            "\n== Fields (3) ==\n\n",
            "id: UnitId  struct Unit  a.rs\n",
            "probe: Vec<&'static str>  struct Unit  a.rs\n",
            "target: Entity  variant Mode::Attacking  a.rs\n",
            "\n== Variants (3) ==\n\n",
            "Attacking  enum Mode  a.rs\n",
            "Idle  enum Mode  a.rs\n",
            "Moving  enum Mode  a.rs\n",
            "\n== Values (3) ==\n\n",
            "COUNT: u32  static  a.rs\n",
            "MAX: u8  impl Unit<u8>  a.rs\n",
            "NAME: &'static str  trait Module  a.rs\n",
        );
        assert!(entries.starts_with(expected), "{entries}");
    }
}
