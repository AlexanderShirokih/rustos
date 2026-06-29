//! Proc-macro `#[protocol]`: кодоген IPC-контрактов поверх рантайма `ipc`.
//!
//! Атрибут на trait объявляет протокол; методы с
//! `#[call]`/`#[cast]`/`#[event]` - его операции. Макрос
//! заменяет trait маркер-типом и эмитит client/service/dispatch/events,
//! ordinal-константы и дескриптор схемы. Хэш ordinal считается
//! host-стороной (`sha2`); рантайм хранит готовые литералы.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{DeriveInput, Error, ItemTrait, Meta, Token, parse_macro_input, punctuated::Punctuated};

mod codegen;
mod model;
mod ordinal;
mod wire_value;

/// Объявляет IPC-протокол:
/// `#[protocol(name = "...", timeout_ns = N, transport = "...")]` на trait.
/// `name` - неймспейс ordinal (по умолчанию имя trait). `timeout_ns` - дефолтный
/// срок ожидания ответа two-way `#[call]` (нс; по умолчанию `u64::MAX` -
/// бессрочно), перекрываемый на операции через `#[call(timeout_ns = N)]`.
/// `transport` - плоскость: `"port"` (по умолчанию) или `"ring"` (SPSC-кольцо,
/// допускает только `#[cast]`/`#[event]`).
/// Операции размечаются `#[call]` (two-way), `#[cast]`, `#[event]`.
#[proc_macro_attribute]
pub fn protocol(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr with Punctuated::<Meta, Token![,]>::parse_terminated);
    let item = parse_macro_input!(item as ItemTrait);

    expand(&args, &item)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// Выводит `WireValue`/`WireTyped` для struct с именованными полями (каждое -
/// `WireValue`). Значение укладывается во вложенный суб-кадр одного wire-поля;
/// дескриптор - `WireType::Aggregate` из полей. Допускает ноль или один лайфтайм;
/// type/const-генерики и более одного лайфтайма отвергаются.
#[proc_macro_derive(WireValue)]
pub fn derive_wire_value(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    wire_value::expand(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand(args: &Punctuated<Meta, Token![,]>, item: &ItemTrait) -> syn::Result<TokenStream2> {
    let protocol = model::build(args, item)?;
    let generated = codegen::expand(&protocol);
    let marker = marker_type(item);

    Ok(quote! {
        #marker
        #generated
    })
}

fn marker_type(item: &ItemTrait) -> TokenStream2 {
    let vis = &item.vis;
    let ident = &item.ident;
    let docs = item.attrs.iter().filter(|attr| attr.path().is_ident("doc"));
    quote! {
        #(#docs)*
        #vis enum #ident {}
    }
}
