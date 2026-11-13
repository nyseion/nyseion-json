//! `#[derive(JsonWrite)]` for `rimu-json`.
//!
//! The generated code writes every constant piece of the output (braces, keys with their quotes
//! and colons, the commas between fields, the quotes around `String` and `&str` fields) as one
//! merged literal per run, so a struct of plain fields becomes alternating literal copies and value
//! writes. It reads `#[serde(...)]` and `#[json(...)]` attributes with serde's meaning. Attributes
//! that change the output in a way this derive does not reproduce are compile errors, so the
//! output is always the bytes `serde_json` writes for the same type.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::{
  Attribute, Data, DeriveInput, ExprPath, Fields, GenericParam, LitStr, Token, Type, ext::IdentExt,
  meta::ParseNestedMeta, parse_macro_input, parse_quote, spanned::Spanned,
};

#[proc_macro_derive(JsonWrite, attributes(json, serde))]
pub fn derive_json_write(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  expand(input).unwrap_or_else(syn::Error::into_compile_error).into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rule {
  None,
  Lower,
  Upper,
  Pascal,
  Camel,
  Snake,
  ScreamingSnake,
  Kebab,
  ScreamingKebab,
}

impl Rule {
  fn parse(lit: &LitStr) -> syn::Result<Self> {
    Ok(match lit.value().as_str() {
      "lowercase" => Rule::Lower,
      "UPPERCASE" => Rule::Upper,
      "PascalCase" => Rule::Pascal,
      "camelCase" => Rule::Camel,
      "snake_case" => Rule::Snake,
      "SCREAMING_SNAKE_CASE" => Rule::ScreamingSnake,
      "kebab-case" => Rule::Kebab,
      "SCREAMING-KEBAB-CASE" => Rule::ScreamingKebab,
      _ => return Err(syn::Error::new(lit.span(), "unknown rename rule")),
    })
  }

  fn or(self, other: Self) -> Self {
    if self == Rule::None { other } else { self }
  }

  /// As serde_derive's `RenameRule::apply_to_variant`.
  fn variant(self, name: &str) -> String {
    match self {
      Rule::None | Rule::Pascal => name.to_owned(),
      Rule::Lower => name.to_ascii_lowercase(),
      Rule::Upper => name.to_ascii_uppercase(),
      Rule::Camel => name[..1].to_ascii_lowercase() + &name[1..],
      Rule::Snake => {
        let mut snake = String::new();
        for (idx, ch) in name.char_indices() {
          if idx > 0 && ch.is_uppercase() {
            snake.push('_');
          }
          snake.push(ch.to_ascii_lowercase());
        }
        snake
      }
      Rule::ScreamingSnake => Rule::Snake.variant(name).to_ascii_uppercase(),
      Rule::Kebab => Rule::Snake.variant(name).replace('_', "-"),
      Rule::ScreamingKebab => Rule::ScreamingSnake.variant(name).replace('_', "-"),
    }
  }

  /// As serde_derive's `RenameRule::apply_to_field`.
  fn field(self, name: &str) -> String {
    match self {
      Rule::None | Rule::Lower | Rule::Snake => name.to_owned(),
      Rule::Upper | Rule::ScreamingSnake => name.to_ascii_uppercase(),
      Rule::Pascal => {
        let mut pascal = String::new();
        let mut capitalize = true;
        for ch in name.chars() {
          if ch == '_' {
            capitalize = true;
          } else if capitalize {
            pascal.push(ch.to_ascii_uppercase());
            capitalize = false;
          } else {
            pascal.push(ch);
          }
        }
        pascal
      }
      Rule::Camel => {
        let pascal = Rule::Pascal.field(name);
        pascal[..1].to_ascii_lowercase() + &pascal[1..]
      }
      Rule::Kebab => name.replace('_', "-"),
      Rule::ScreamingKebab => Rule::ScreamingSnake.field(name).replace('_', "-"),
    }
  }
}

/// Where an attribute list sits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Site {
  Container,
  Variant,
  Field,
}

#[derive(Default)]
struct Attrs {
  rename: Option<String>,
  rename_all: Option<Rule>,
  rename_all_fields: Option<Rule>,
  skip: bool,
  skip_if: Option<ExprPath>,
  transparent: bool,
}

/// Attributes that only affect deserialization, or that do not change the output.
const IGNORED: &[&str] = &[
  "alias",
  "borrow",
  "bound",
  "crate",
  "default",
  "deny_unknown_fields",
  "deserialize_with",
  "expecting",
  "from",
  "other",
  "skip_deserializing",
  "try_from",
];

/// Parses the `#[serde(...)]` and `#[json(...)]` attributes of one site.
fn parse_attrs(attrs: &[Attribute], site: Site) -> syn::Result<Attrs> {
  let mut out = Attrs::default();
  for attr in attrs {
    if !attr.path().is_ident("serde") && !attr.path().is_ident("json") {
      continue;
    }
    attr.parse_nested_meta(|meta| {
      let name = meta.path.get_ident().map(ToString::to_string).unwrap_or_default();
      match (name.as_str(), site) {
        ("rename", Site::Container) => {
          // The name of the type does not appear in the output.
          skip_value(&meta)
        }
        ("rename", _) => {
          if let Some(name) = parse_serialize_str(&meta)? {
            out.rename = Some(name.value());
          }
          Ok(())
        }
        ("rename_all", Site::Container | Site::Variant) => {
          if let Some(lit) = parse_serialize_str(&meta)? {
            out.rename_all = Some(Rule::parse(&lit)?);
          }
          Ok(())
        }
        ("rename_all_fields", Site::Container) => {
          if let Some(lit) = parse_serialize_str(&meta)? {
            out.rename_all_fields = Some(Rule::parse(&lit)?);
          }
          Ok(())
        }
        ("skip" | "skip_serializing", Site::Field) => {
          out.skip = true;
          Ok(())
        }
        ("skip_serializing_if", Site::Field) => {
          let lit: LitStr = meta.value()?.parse()?;
          out.skip_if = Some(lit.parse()?);
          Ok(())
        }
        ("transparent", Site::Container) => {
          out.transparent = true;
          Ok(())
        }
        (ignored, _) if IGNORED.contains(&ignored) => skip_value(&meta),
        _ => Err(meta.error(format!(
          "`{name}` is not supported by `#[derive(JsonWrite)]` here: its output would differ from \
           serde_json's; write `JsonWrite` by hand for this type"
        ))),
      }
    })?;
  }
  Ok(out)
}

/// `name = "x"` or `name(serialize = "x", deserialize = "y")`: the serialize name, if any.
fn parse_serialize_str(meta: &ParseNestedMeta<'_>) -> syn::Result<Option<LitStr>> {
  if meta.input.peek(Token![=]) {
    return Ok(Some(meta.value()?.parse()?));
  }
  let mut found = None;
  meta.parse_nested_meta(|inner| {
    let lit: LitStr = inner.value()?.parse()?;
    if inner.path.is_ident("serialize") {
      found = Some(lit);
    } else if !inner.path.is_ident("deserialize") {
      return Err(inner.error("expected `serialize` or `deserialize`"));
    }
    Ok(())
  })?;
  Ok(found)
}

/// Consumes `= value`, `(...)` or nothing.
fn skip_value(meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
  if meta.input.peek(Token![=]) {
    let _: syn::Expr = meta.value()?.parse()?;
  } else if meta.input.peek(syn::token::Paren) {
    meta.parse_nested_meta(|inner| skip_value(&inner))?;
  }
  Ok(())
}

/// `name` as the contents of a JSON string, escaped as serde_json escapes.
fn json_escape(name: &str) -> String {
  let mut out = String::with_capacity(name.len());
  for ch in name.chars() {
    match ch {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\u{08}' => out.push_str("\\b"),
      '\t' => out.push_str("\\t"),
      '\n' => out.push_str("\\n"),
      '\u{0C}' => out.push_str("\\f"),
      '\r' => out.push_str("\\r"),
      ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
      ch => out.push(ch),
    }
  }
  out
}

/// Statements that write to `__w`, with adjacent literal pieces merged into one `raw`.
struct Emitter {
  pending: String,
  stmts: Vec<TokenStream2>,
}

impl Emitter {
  fn new() -> Self {
    Self { pending: String::new(), stmts: Vec::new() }
  }

  fn lit(&mut self, text: &str) {
    self.pending.push_str(text);
  }

  fn stmt(&mut self, stmt: TokenStream2) {
    self.flush();
    self.stmts.push(stmt);
  }

  fn flush(&mut self) {
    if !self.pending.is_empty() {
      let lit = LitStr::new(&self.pending, Span::call_site());
      self.stmts.push(quote!(__w.raw(#lit);));
      self.pending.clear();
    }
  }

  fn finish(mut self) -> TokenStream2 {
    self.flush();
    let stmts = self.stmts;
    quote!(#(#stmts)*)
  }

  /// A value. `String` and `&str` become string contents between quotes merged into the
  /// surrounding literals; anything else goes through `JsonWrite`. `expr` is a reference.
  fn value(&mut self, expr: &TokenStream2, ty: &Type) {
    if is_str_like(ty) {
      self.lit("\"");
      self.stmt(quote!(__w.str_contents(::rimu_json::__private::AsJsonStr::as_json_str(#expr));));
      self.lit("\"");
    } else {
      self.stmt(quote!(::rimu_json::JsonWrite::write_json(#expr, __w);));
    }
  }
}

/// `String`, `&str` and `&String`, by name. A different type with one of these names fails to
/// compile against `AsJsonStr` rather than writing something else.
fn is_str_like(ty: &Type) -> bool {
  match ty {
    Type::Path(path) if path.qself.is_none() => {
      path.path.segments.last().is_some_and(|seg| seg.ident == "String" && seg.arguments.is_empty())
    }
    Type::Reference(reference) => match &*reference.elem {
      Type::Path(path) if path.qself.is_none() => {
        path.path.is_ident("str") || is_str_like(&reference.elem)
      }
      _ => false,
    },
    Type::Group(group) => is_str_like(&group.elem),
    Type::Paren(paren) => is_str_like(&paren.elem),
    _ => false,
  }
}

/// A field that is written: its key, type, how to reach it, and its skip condition.
struct Field {
  key: String,
  ty: Type,
  expr: TokenStream2,
  skip_if: Option<ExprPath>,
}

/// Comma state between the fields of an object, as far as it is known at compile time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Comma {
  /// Nothing written yet.
  None,
  /// Something written: the next field starts with a comma.
  Some,
  /// Depends on skipped fields: the runtime flag `__any` says.
  Dynamic,
}

/// The fields of an object, after its `{`, and its `}`.
fn object_fields(em: &mut Emitter, fields: &[Field]) {
  let mut comma = Comma::None;
  for field in fields {
    let key = format!("\"{}\":", json_escape(&field.key));
    let expr = &field.expr;
    match &field.skip_if {
      None => {
        match comma {
          Comma::None => em.lit(&key),
          Comma::Some => em.lit(&format!(",{key}")),
          Comma::Dynamic => {
            em.stmt(quote!(if __any {
              __w.raw(",");
            }));
            em.lit(&key);
          }
        }
        em.value(expr, &field.ty);
        comma = Comma::Some;
      }
      Some(path) => {
        let mut body = Emitter::new();
        match comma {
          Comma::None => body.lit(&key),
          Comma::Some => body.lit(&format!(",{key}")),
          Comma::Dynamic => {
            body.stmt(quote!(if __any {
              __w.raw(",");
            }));
            body.lit(&key);
          }
        }
        body.value(expr, &field.ty);
        let body = body.finish();
        if comma == Comma::None {
          em.stmt(quote!(let mut __any = false;));
        }
        let set = if comma == Comma::Some { quote!() } else { quote!(__any = true;) };
        em.stmt(quote!(if !#path(#expr) { #body #set }));
        if comma == Comma::None {
          comma = Comma::Dynamic;
        }
      }
    }
  }
  em.lit("}");
}

/// The fields of a declaration, with their attributes. `expr` gives the expression that reaches
/// field `idx` (a reference).
fn collect_fields(
  fields: &Fields,
  rule: Rule,
  expr: impl Fn(usize, &syn::Field) -> TokenStream2,
) -> syn::Result<Vec<(Attrs, Field)>> {
  let mut out = Vec::new();
  for (idx, field) in fields.iter().enumerate() {
    let attrs = parse_attrs(&field.attrs, Site::Field)?;
    let key = match (&attrs.rename, &field.ident) {
      (Some(name), _) => name.clone(),
      (None, Some(ident)) => rule.field(&ident.unraw().to_string()),
      (None, None) => String::new(),
    };
    let skip_if = attrs.skip_if.clone();
    out.push((attrs, Field { key, ty: field.ty.clone(), expr: expr(idx, field), skip_if }));
  }
  Ok(out)
}

/// The body for fields of any shape: an object, a newtype's value, an array, or `null`.
fn fields_body(
  em: &mut Emitter,
  fields: &Fields,
  collected: Vec<(Attrs, Field)>,
) -> syn::Result<()> {
  match fields {
    Fields::Named(_) => {
      em.lit("{");
      let kept: Vec<Field> =
        collected.into_iter().filter(|(attrs, _)| !attrs.skip).map(|(_, field)| field).collect();
      object_fields(em, &kept);
    }
    Fields::Unnamed(unnamed) => {
      for (attrs, _) in &collected {
        if attrs.skip_if.is_some() {
          return Err(syn::Error::new(
            unnamed.span(),
            "`skip_serializing_if` on a tuple field is not supported by `#[derive(JsonWrite)]`",
          ));
        }
      }
      if collected.len() == 1 {
        let (attrs, field) = &collected[0];
        if attrs.skip {
          return Err(syn::Error::new(unnamed.span(), "a newtype's only field cannot be skipped"));
        }
        em.value(&field.expr, &field.ty);
      } else {
        em.lit("[");
        let mut first = true;
        for (attrs, field) in &collected {
          if attrs.skip {
            continue;
          }
          if !first {
            em.lit(",");
          }
          first = false;
          em.value(&field.expr, &field.ty);
        }
        em.lit("]");
      }
    }
    Fields::Unit => em.lit("null"),
  }
  Ok(())
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
  let container = parse_attrs(&input.attrs, Site::Container)?;
  let name = &input.ident;
  let mut em = Emitter::new();
  match &input.data {
    Data::Struct(data) => {
      let rule = container.rename_all.unwrap_or(Rule::None);
      let collected = collect_fields(&data.fields, rule, |idx, field| match &field.ident {
        Some(ident) => quote!(&self.#ident),
        None => {
          let index = syn::Index::from(idx);
          quote!(&self.#index)
        }
      })?;
      if container.transparent {
        let mut kept = collected.into_iter().filter(|(attrs, _)| !attrs.skip);
        match (kept.next(), kept.next()) {
          (Some((_, field)), None) => em.value(&field.expr, &field.ty),
          _ => {
            return Err(syn::Error::new(
              input.span(),
              "`transparent` needs exactly one field that is not skipped",
            ));
          }
        }
      } else {
        fields_body(&mut em, &data.fields, collected)?;
      }
    }
    Data::Enum(data) => {
      if container.transparent {
        return Err(syn::Error::new(input.span(), "`transparent` is for structs"));
      }
      let mut arms = Vec::new();
      for variant in &data.variants {
        let attrs = parse_attrs(&variant.attrs, Site::Variant)?;
        let tag = attrs.rename.clone().unwrap_or_else(|| {
          container.rename_all.unwrap_or(Rule::None).variant(&variant.ident.unraw().to_string())
        });
        let tag = json_escape(&tag);
        let vident = &variant.ident;
        let rule = attrs
          .rename_all
          .unwrap_or(Rule::None)
          .or(container.rename_all_fields.unwrap_or(Rule::None));
        let binding = |idx: usize| format_ident!("__f{}", idx);
        let collected = collect_fields(&variant.fields, rule, |idx, _| {
          let id = binding(idx);
          quote!(#id)
        })?;
        let mut arm = Emitter::new();
        let pattern = match &variant.fields {
          Fields::Unit => {
            arm.lit(&format!("\"{tag}\""));
            quote!(Self::#vident)
          }
          Fields::Named(named) => {
            let binds: Vec<_> = named
              .named
              .iter()
              .zip(&collected)
              .enumerate()
              .filter(|(_, (_, (attrs, _)))| !attrs.skip)
              .map(|(idx, (field, _))| {
                let ident = field.ident.as_ref().expect("named");
                let id = binding(idx);
                quote!(#ident: #id)
              })
              .collect();
            arm.lit(&format!("{{\"{tag}\":"));
            fields_body(&mut arm, &variant.fields, collected)?;
            arm.lit("}");
            quote!(Self::#vident { #(#binds,)* .. })
          }
          Fields::Unnamed(unnamed) => {
            let binds =
              unnamed.unnamed.iter().zip(&collected).enumerate().map(|(idx, (_, (attrs, _)))| {
                if attrs.skip {
                  quote!(_)
                } else {
                  let id = binding(idx);
                  quote!(#id)
                }
              });
            let binds: Vec<_> = binds.collect();
            arm.lit(&format!("{{\"{tag}\":"));
            if unnamed.unnamed.is_empty() {
              arm.lit("[]");
            } else {
              fields_body(&mut arm, &variant.fields, collected)?;
            }
            arm.lit("}");
            quote!(Self::#vident(#(#binds),*))
          }
        };
        let body = arm.finish();
        arms.push(quote!(#pattern => { #body }));
      }
      if arms.is_empty() {
        em.stmt(quote!(match *self {}));
      } else {
        em.stmt(quote!(match self { #(#arms)* }));
      }
    }
    Data::Union(_) => {
      return Err(syn::Error::new(input.span(), "`JsonWrite` cannot be derived for a union"));
    }
  }
  let body = em.finish();
  let mut generics = input.generics.clone();
  let bounded: Vec<_> = generics
    .params
    .iter()
    .filter_map(|param| match param {
      GenericParam::Type(ty) => Some(ty.ident.clone()),
      _ => None,
    })
    .collect();
  let where_clause = generics.make_where_clause();
  for ident in bounded {
    where_clause.predicates.push(parse_quote!(#ident: ::rimu_json::JsonWrite));
  }
  let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
  Ok(quote! {
    #[automatically_derived]
    impl #impl_generics ::rimu_json::JsonWrite for #name #ty_generics #where_clause {
      #[inline]
      #[allow(unused_assignments, unused_mut, unused_variables, clippy::all)]
      fn write_json(&self, __w: &mut ::rimu_json::JsonWriter<'_>) {
        #body
      }
    }
  })
}
