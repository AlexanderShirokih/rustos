//! Вывод `#[derive(WireValue)]` для пользовательских struct.
//!
//! Значение агрегата укладывается во вложенный суб-кадр одного wire-поля:
//! `MessageBuf<FIELD_DATA_MAX>`, поля по позиционному индексу `1..`, терминатор.
//! Суб-кадр сверх `FIELD_DATA_MAX` даёт `IpcError` из `write_field`/`finish` -
//! без паники. Вложенность работает рекурсивно: поле-агрегат пишет свой суб-кадр.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Error, Fields, GenericParam, Ident, Lifetime, LifetimeParam, Type,
    spanned::Spanned,
};

/// Верхняя граница позиционного `field_id` суб-кадра (`wire::FIELD_ID_MAX`).
const FIELD_ID_MAX: u8 = 254;

/// Именованное поле агрегата: имя, позиционный `field_id` и тип.
struct AggField {
    ident: Ident,
    field_id: u8,
    ty: Type,
}

/// Разворачивает derive в реализации `WireValue` и `WireTyped`.
pub fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let fields = collect_fields(input)?;
    let wire_lifetime = resolve_lifetime(input)?;

    let write = expand_write(&fields);
    let read = expand_read(&fields);
    let desc = expand_desc(&fields);

    let ident = &input.ident;
    // type-форма имени с лайфтаймом, если он у структуры есть.
    let (impl_wire, impl_typed) = match &wire_lifetime {
        Some(lt) => (
            quote!(impl<#lt> ::ipc::WireValue<#lt> for #ident<#lt>),
            quote!(impl<#lt> ::ipc::WireTyped for #ident<#lt>),
        ),
        None => (
            quote!(impl<'__wire> ::ipc::WireValue<'__wire> for #ident),
            quote!(impl ::ipc::WireTyped for #ident),
        ),
    };
    let read_lifetime = wire_lifetime
        .clone()
        .unwrap_or_else(|| Lifetime::new("'__wire", Span::call_site()));

    Ok(quote! {
        #impl_wire {
            fn write_as_field<const __N: usize>(
                &self,
                __buf: &mut ::ipc::wire::MessageBuf<__N>,
                __field_id: u8,
            ) -> ::core::result::Result<(), ::ipc::wire::IpcError> {
                let mut __sub = ::ipc::wire::MessageBuf::<{ ::ipc::wire::FIELD_DATA_MAX }>::new();
                #write
                __sub.finish()?;
                __buf.write_field(__field_id, __sub.as_bytes())
            }

            fn from_field(
                __data: &#read_lifetime [u8],
            ) -> ::core::result::Result<Self, ::ipc::wire::IpcError> {
                #read
            }
        }

        #impl_typed {
            const WIRE_TYPE: ::ipc::schema::WireType = #desc;
        }
    })
}

fn collect_fields(input: &DeriveInput) -> syn::Result<Vec<AggField>> {
    let Data::Struct(data) = &input.data else {
        return Err(Error::new(
            input.span(),
            "#[derive(WireValue)] supports a struct with named fields",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(Error::new(
            data.fields.span(),
            "#[derive(WireValue)] supports a struct with named fields",
        ));
    };

    let mut fields = Vec::with_capacity(named.named.len());
    for (index, field) in named.named.iter().enumerate() {
        let ident = field.ident.clone().ok_or_else(|| {
            Error::new(field.span(), "#[derive(WireValue)] field is a named field")
        })?;
        // Позиционный field_id 1.. строго возрастает, потолок - FIELD_ID_MAX.
        let field_id = u8::try_from(index + 1)
            .ok()
            .filter(|id| *id <= FIELD_ID_MAX)
            .ok_or_else(|| {
                Error::new(
                    field.span(),
                    "#[derive(WireValue)] struct has too many fields",
                )
            })?;
        fields.push(AggField {
            ident,
            field_id,
            ty: field.ty.clone(),
        });
    }
    Ok(fields)
}

/// Лайфтайм для `WireValue<'a>`: единственный у структуры либо отсутствует.
/// type/const-генерики и более одного лайфтайма не поддержаны.
fn resolve_lifetime(input: &DeriveInput) -> syn::Result<Option<Lifetime>> {
    let mut lifetime: Option<&LifetimeParam> = None;
    for param in &input.generics.params {
        match param {
            GenericParam::Lifetime(lt) => {
                if lifetime.is_some() {
                    return Err(Error::new(
                        lt.span(),
                        "#[derive(WireValue)] supports at most one lifetime",
                    ));
                }
                lifetime = Some(lt);
            }
            GenericParam::Type(ty) => {
                return Err(Error::new(
                    ty.span(),
                    "#[derive(WireValue)] does not support a type generic",
                ));
            }
            GenericParam::Const(c) => {
                return Err(Error::new(
                    c.span(),
                    "#[derive(WireValue)] does not support a const generic",
                ));
            }
        }
    }
    Ok(lifetime.map(|lt| lt.lifetime.clone()))
}

fn expand_write(fields: &[AggField]) -> TokenStream {
    let writes = fields.iter().map(|field| {
        let ident = &field.ident;
        let field_id = field.field_id;
        quote! {
            ::ipc::WireValue::write_as_field(&self.#ident, &mut __sub, #field_id)?;
        }
    });
    quote! { #(#writes)* }
}

fn expand_read(fields: &[AggField]) -> TokenStream {
    let decls = fields.iter().map(|field| {
        let opt = format_ident!("__opt_{}", field.ident);
        let ty = &field.ty;
        quote! {
            let mut #opt: ::core::option::Option<#ty> = ::core::option::Option::None;
        }
    });
    let arms = fields.iter().map(|field| {
        let opt = format_ident!("__opt_{}", field.ident);
        let field_id = field.field_id;
        let ty = &field.ty;
        quote! {
            #field_id => {
                #opt = ::core::option::Option::Some(
                    <#ty as ::ipc::WireValue>::from_field(__field.data)?,
                );
            }
        }
    });
    let binds = fields.iter().map(|field| {
        let ident = &field.ident;
        let opt = format_ident!("__opt_{}", field.ident);
        quote! {
            #ident: match #opt {
                ::core::option::Option::Some(__v) => __v,
                ::core::option::Option::None => {
                    return ::core::result::Result::Err(::ipc::wire::IpcError::MissingField);
                }
            }
        }
    });
    quote! {
        #(#decls)*
        let mut __cursor = ::ipc::wire::FieldCursor::new(__data);
        while let ::core::option::Option::Some(__field) = __cursor.next_field()? {
            match __field.id {
                #(#arms)*
                _ => {}
            }
        }
        ::core::result::Result::Ok(Self {
            #(#binds),*
        })
    }
}

fn expand_desc(fields: &[AggField]) -> TokenStream {
    let entries = fields.iter().map(|field| {
        let ty = &field.ty;
        quote!(<#ty as ::ipc::WireTyped>::WIRE_TYPE)
    });
    quote! {
        ::ipc::schema::WireType::Aggregate(&[ #(#entries),* ])
    }
}
