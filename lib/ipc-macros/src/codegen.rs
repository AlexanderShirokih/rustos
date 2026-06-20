//! Кодогенерация client/service/dispatch/events и дескриптора схемы.
//!
//! По модели контракта эмитятся: маркер `Protocol`, клиентский proxy над
//! `Transport`, серверный trait сервиса, `dispatch` одного кадра, клиентский
//! trait событий с `dispatch_event`, серверный эмиттер событий, модуль ordinal
//! и дескриптор `DESC`.

use ipc_schema::Kind;
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::Ident;

use crate::model::{Operation, Param, Protocol, WireTy};

fn op_docs(op: &Operation) -> TokenStream {
    let docs = &op.docs;
    quote! { #(#docs)* }
}

/// Раскрывает модель протокола в полный набор генерируемых сущностей.
pub fn expand(protocol: &Protocol) -> TokenStream {
    let marker = expand_marker(protocol);
    let client = expand_client(protocol);
    let service = expand_service(protocol);
    let dispatch = expand_dispatch(protocol);
    let events = expand_events(protocol);
    let ordinal_mod = expand_ordinal_module(protocol);
    let budget = expand_budget_asserts(protocol);

    quote! {
        #marker
        #client
        #service
        #dispatch
        #events
        #ordinal_mod
        #budget
    }
}

/// Для операций с не-литеральной границей бюджет кадра нельзя проверить на
/// этапе раскрытия макроса; эмитим эквивалентный `const`-assert, где значения
/// границ уже резолвятся.
fn expand_budget_asserts(protocol: &Protocol) -> TokenStream {
    let asserts = protocol
        .operations
        .iter()
        .filter(|op| has_dynamic_bound(op))
        .map(|op| {
            let terms = op.params.iter().map(|param| {
                let data = data_len_expr(&param.ty);
                quote!(+ ::ipc::wire::FIELD_OVERHEAD + #data)
            });
            quote! {
                const _: () = ::core::assert!(
                    1usize #(#terms)* <= ::ipc::wire::BODY_MAX,
                    "operation frame exceeds the MESSAGE_INLINE_MAX budget"
                );
            }
        });
    quote! { #(#asserts)* }
}

fn has_dynamic_bound(op: &Operation) -> bool {
    op.params
        .iter()
        .any(|param| matches!(&param.ty, WireTy::Str(b) | WireTy::Bytes(b) if b.lit.is_none()))
}

/// Длина `data` поля как const-выражение для бюджет-assert'а.
fn data_len_expr(ty: &WireTy) -> TokenStream {
    match ty {
        WireTy::Uint(width) | WireTy::Int(width) => {
            let width = *width as usize;
            quote!(#width)
        }
        WireTy::Bool | WireTy::Cap => quote!(1usize),
        WireTy::Str(bound) | WireTy::Bytes(bound) => {
            let n = &bound.value;
            quote!(#n)
        }
    }
}

fn wire_ty_tokens(ty: &WireTy, lifetime: &TokenStream) -> TokenStream {
    match ty {
        WireTy::Uint(1) => quote!(u8),
        WireTy::Uint(2) => quote!(u16),
        WireTy::Uint(4) => quote!(u32),
        WireTy::Uint(8) => quote!(u64),
        WireTy::Uint(_) => unreachable!("uint width validated by the model"),
        WireTy::Int(1) => quote!(i8),
        WireTy::Int(2) => quote!(i16),
        WireTy::Int(4) => quote!(i32),
        WireTy::Int(8) => quote!(i64),
        WireTy::Int(_) => unreachable!("int width validated by the model"),
        WireTy::Bool => quote!(bool),
        WireTy::Str(bound) => {
            let n = &bound.expr;
            quote!(::ipc::wire::Str<#lifetime, #n>)
        }
        WireTy::Bytes(bound) => {
            let n = &bound.expr;
            quote!(::ipc::wire::Bytes<#lifetime, #n>)
        }
        WireTy::Cap => quote!(::ipc::wire::Cap),
    }
}

fn wire_ty_desc(ty: &WireTy) -> TokenStream {
    match ty {
        WireTy::Uint(width) => quote!(::ipc::schema::WireType::Uint(#width)),
        WireTy::Int(width) => quote!(::ipc::schema::WireType::Int(#width)),
        WireTy::Bool => quote!(::ipc::schema::WireType::Bool),
        WireTy::Str(bound) => {
            let n = &bound.value;
            quote!(::ipc::schema::WireType::BoundedStr(#n))
        }
        WireTy::Bytes(bound) => {
            let n = &bound.value;
            quote!(::ipc::schema::WireType::BoundedBytes(#n))
        }
        WireTy::Cap => quote!(::ipc::schema::WireType::Capability),
    }
}

fn expand_marker(protocol: &Protocol) -> TokenStream {
    let trait_ident = &protocol.trait_ident;
    quote! {
        impl ::ipc::wire::Protocol for #trait_ident {}
    }
}

fn suffixed(trait_ident: &Ident, suffix: &str) -> Ident {
    format_ident!("{}{}", trait_ident, suffix)
}

fn decode_single_value(ty: &WireTy, body: &TokenStream) -> TokenStream {
    let ty_tokens = wire_ty_tokens(ty, &quote!('_));
    let decode = decode_field_value(ty);
    quote! {{
        let mut __cursor = ::ipc::wire::FieldCursor::new(#body);
        let mut __value: ::core::option::Option<#ty_tokens> = ::core::option::Option::None;
        while let ::core::option::Option::Some(__field) = __cursor.next_field()? {
            if __field.id == 1u8 {
                __value = ::core::option::Option::Some(#decode);
            }
        }
        match __value {
            ::core::option::Option::Some(__v) => __v,
            ::core::option::Option::None => {
                return ::core::result::Result::Err(::ipc::wire::IpcError::MissingField);
            }
        }
    }}
}

/// Декод одного значения поля; для Cap читает индекс и достаёт id из `__in_handles`,
/// иначе - обычный `WireValue::from_field`. Опирается на `__field`/`__in_handles` в области.
fn decode_field_value(ty: &WireTy) -> TokenStream {
    if let WireTy::Cap = ty {
        quote! {{
            let __idx = ::ipc::wire::value::decode_port_index(__field.data)?;
            let __raw = *__in_handles
                .get(__idx as usize)
                .ok_or(::ipc::wire::IpcError::BadLength)?;
            let __nz = ::core::num::NonZeroU32::new(__raw)
                .ok_or(::ipc::wire::IpcError::BadLength)?;
            ::ipc::wire::Cap::from_raw(__nz)
        }}
    } else {
        let ty_tokens = wire_ty_tokens(ty, &quote!('_));
        quote!(<#ty_tokens as ::ipc::WireValue>::from_field(__field.data)?)
    }
}

/// Запись одного значения в `buf`; для Cap пишет индекс и кладёт id в `__out_handles`,
/// иначе - обычный `WireValue::write_as_field`. Опирается на `__out_handles`/`__out_handle_count`.
fn encode_field(value: &TokenStream, field_id: u8, ty: &WireTy, buf: &TokenStream) -> TokenStream {
    if let WireTy::Cap = ty {
        quote! {
            #buf.write_field(
                #field_id,
                &::ipc::wire::value::encode_port_index(__out_handle_count as u8),
            )?;
            __out_handles[__out_handle_count] = #value.raw().get();
            __out_handle_count += 1;
        }
    } else {
        quote! {
            ::ipc::WireValue::write_as_field(&#value, &mut #buf, #field_id)?;
        }
    }
}

/// Эмитит запись параметров запроса в `__buf` и аргумент handle-среза для `write_message`.
/// Массив `__out_handles` объявляется только при наличии Cap-поля.
fn encode_params(params: &[Param]) -> (TokenStream, TokenStream) {
    let has_cap = params.iter().any(|param| matches!(param.ty, WireTy::Cap));
    let writes = params.iter().map(|param| {
        let ident = &param.ident;
        encode_field(&quote!(#ident), param.field_id, &param.ty, &quote!(__buf))
    });
    let decl = handles_out_decl(has_cap);
    let handles_arg = handles_out_arg(has_cap);
    (quote! { #decl #(#writes)* }, handles_arg)
}

/// Эмитит запись возвращаемого значения `value` в `buf` и аргумент handle-среза.
fn encode_return(
    value: &TokenStream,
    ty: &WireTy,
    buf: &TokenStream,
) -> (TokenStream, TokenStream) {
    let has_cap = matches!(ty, WireTy::Cap);
    let decl = handles_out_decl(has_cap);
    let write = encode_field(value, 1u8, ty, buf);
    (quote! { #decl #write }, handles_out_arg(has_cap))
}

fn handles_out_decl(has_cap: bool) -> TokenStream {
    if has_cap {
        quote! {
            let mut __out_handles = [0u32; ::ipc::wire::MESSAGE_MAX_HANDLES];
            let mut __out_handle_count = 0usize;
        }
    } else {
        TokenStream::new()
    }
}

fn handles_out_arg(has_cap: bool) -> TokenStream {
    if has_cap {
        quote!(&__out_handles[..__out_handle_count])
    } else {
        quote!(&[])
    }
}

fn param_signature(params: &[Param]) -> TokenStream {
    let parts = params.iter().map(|param| {
        let ident = &param.ident;
        let ty = wire_ty_tokens(&param.ty, &quote!('_));
        quote!(#ident: #ty)
    });
    quote! {
        #(, #parts)*
    }
}

fn expand_client(protocol: &Protocol) -> TokenStream {
    let vis = &protocol.vis;
    let trait_ident = &protocol.trait_ident;
    let client_ident = suffixed(trait_ident, "Client");

    let methods = protocol
        .operations
        .iter()
        .filter(|op| matches!(op.kind, Kind::Call | Kind::Cast))
        .map(|op| expand_client_method(protocol, op));

    // Дефолт `wait_ns` из `#[protocol(timeout_ns = N)]`; иначе бессрочно.
    let default_wait_ns = protocol
        .default_timeout_ns
        .map_or_else(|| quote!(u64::MAX), |ns| quote!(#ns));

    quote! {
        #vis struct #client_ident<T: ::ipc::Transport> {
            transport: T,
            next_txid: ::core::cell::Cell<u32>,
            wait_ns: u64,
        }

        impl<T: ::ipc::Transport> #client_ident<T> {
            #vis fn new(transport: T) -> Self {
                Self {
                    transport,
                    next_txid: ::core::cell::Cell::new(1u32),
                    wait_ns: #default_wait_ns,
                }
            }

            /// Переопределяет дефолтный тайм-аут ожидания ответа two-way
            /// `#[call]` (в нс), применяемый к операциям без собственного
            /// `#[call(timeout_ns = N)]`.
            #[must_use]
            #vis fn with_wait_ns(mut self, wait_ns: u64) -> Self {
                self.wait_ns = wait_ns;
                self
            }

            /// Текущий дефолтный тайм-аут ожидания ответа (нс).
            #vis fn wait_ns(&self) -> u64 {
                self.wait_ns
            }

            #vis fn into_transport(self) -> T {
                self.transport
            }

            fn alloc_txid(&self) -> u32 {
                let current = self.next_txid.get();
                let next = match current.checked_add(1) {
                    ::core::option::Option::Some(value) => value,
                    ::core::option::Option::None => 1u32,
                };
                self.next_txid.set(next);
                current
            }

            #(#methods)*
        }
    }
}

fn expand_client_method(protocol: &Protocol, op: &Operation) -> TokenStream {
    let vis = &protocol.vis;
    let method_ident = &op.ident;
    let ordinal = op.ordinal;
    let (encode, encode_handles) = encode_params(&op.params);
    let params_sig = param_signature(&op.params);

    match op.kind {
        Kind::Cast => {
            let docs = op_docs(op);
            quote! {
                #docs
                #vis fn #method_ident(&self #params_sig) -> ::core::result::Result<(), ::ipc::wire::IpcError> {
                    let mut __buf = ::ipc::wire::MessageBuf::<{ ::ipc::wire::MESSAGE_INLINE_MAX }>::new();
                    __buf.write_header(&::ipc::wire::Header::new(#ordinal, 0u32, 0u16))?;
                    #encode
                    __buf.finish()?;
                    self.transport.write_message(__buf.as_bytes(), #encode_handles)
                }
            }
        }
        Kind::Call => {
            let ret = op.ret.as_ref().expect("two-way carries a return type");
            let ret_has_cap = matches!(ret.ok(), WireTy::Cap)
                || ret.err().is_some_and(|err| matches!(err, WireTy::Cap));
            let in_handles_decl = if ret_has_cap {
                quote!(let __in_handles = &__handles[..__len.handles];)
            } else {
                TokenStream::new()
            };
            let ok_ty = wire_ty_tokens(ret.ok(), &quote!('_));
            let (ret_ty, decode_response) = if let Some(err) = ret.err() {
                let err_ty = wire_ty_tokens(err, &quote!('_));
                let decode_ok = decode_single_value(ret.ok(), &quote!(__body));
                let decode_err = decode_single_value(err, &quote!(__body));
                let ret_ty = quote! {
                    ::core::result::Result<
                        ::core::result::Result<#ok_ty, #err_ty>,
                        ::ipc::wire::IpcError,
                    >
                };
                let decode = quote! {
                    if __header.has_flag(::ipc::wire::FLAG_DOMAIN_ERR) {
                        let __err = #decode_err;
                        ::core::result::Result::Ok(::core::result::Result::Err(__err))
                    } else {
                        let __ok = #decode_ok;
                        ::core::result::Result::Ok(::core::result::Result::Ok(__ok))
                    }
                };
                (ret_ty, decode)
            } else {
                let decode_ok = decode_single_value(ret.ok(), &quote!(__body));
                let ret_ty = quote!(::core::result::Result<#ok_ty, ::ipc::wire::IpcError>);
                let decode = quote! {
                    let __ok = #decode_ok;
                    ::core::result::Result::Ok(__ok)
                };
                (ret_ty, decode)
            };

            // Per-call `#[call(timeout_ns = N)]` перекрывает дефолт клиента.
            let wait_ns = op
                .timeout_ns
                .map_or_else(|| quote!(self.wait_ns), |ns| quote!(#ns));

            let docs = op_docs(op);
            quote! {
                #docs
                #vis fn #method_ident(&self #params_sig) -> #ret_ty {
                    let __txid = self.alloc_txid();
                    let mut __buf = ::ipc::wire::MessageBuf::<{ ::ipc::wire::MESSAGE_INLINE_MAX }>::new();
                    __buf.write_header(&::ipc::wire::Header::new(#ordinal, __txid, 0u16))?;
                    #encode
                    __buf.finish()?;
                    self.transport.write_message(__buf.as_bytes(), #encode_handles)?;

                    let mut __bytes = [0u8; ::ipc::wire::MESSAGE_INLINE_MAX];
                    let mut __handles = [0u32; ::ipc::wire::MESSAGE_MAX_HANDLES];
                    loop {
                        self.transport.wait_readable(#wait_ns)?;
                        let __len = self.transport.read_message(&mut __bytes, &mut __handles)?;
                        let __frame = &__bytes[..__len.bytes];
                        let __header = ::ipc::wire::Header::decode(__frame)?;
                        if __header.has_flag(::ipc::wire::FLAG_EPITAPH) {
                            return ::core::result::Result::Err(::ipc::wire::IpcError::PeerClosed);
                        }
                        // ответ обязан нести RESPONSE и тот же txid, иначе кадр отброшен.
                        if !__header.has_flag(::ipc::wire::FLAG_RESPONSE) || __header.txid != __txid {
                            continue;
                        }
                        let __body = &__frame[::ipc::wire::HEADER_SIZE..];
                        #in_handles_decl
                        return { #decode_response };
                    }
                }
            }
        }
        Kind::Event => TokenStream::new(),
    }
}

fn expand_service(protocol: &Protocol) -> TokenStream {
    let vis = &protocol.vis;
    let trait_ident = &protocol.trait_ident;
    let service_ident = suffixed(trait_ident, "Service");

    let methods = protocol
        .operations
        .iter()
        .filter(|op| matches!(op.kind, Kind::Call | Kind::Cast))
        .map(|op| {
            let method_ident = &op.ident;
            let params_sig = param_signature(&op.params);
            match op.kind {
                Kind::Cast => {
                    let docs = op_docs(op);
                    quote! {
                        #docs
                        fn #method_ident(&mut self #params_sig);
                    }
                }
                Kind::Call => {
                    let ret = op.ret.as_ref().expect("two-way carries a return type");
                    let ok_ty = wire_ty_tokens(ret.ok(), &quote!('_));
                    let ret_ty = if let Some(err) = ret.err() {
                        let err_ty = wire_ty_tokens(err, &quote!('_));
                        quote!(::core::result::Result<#ok_ty, #err_ty>)
                    } else {
                        quote!(#ok_ty)
                    };
                    let docs = op_docs(op);
                    quote! {
                        #docs
                        fn #method_ident(&mut self #params_sig) -> #ret_ty;
                    }
                }
                Kind::Event => quote! {},
            }
        });

    quote! {
        #vis trait #service_ident {
            #(#methods)*
        }
    }
}

fn decode_params(params: &[Param]) -> (TokenStream, TokenStream) {
    let decls = params.iter().map(|param| {
        let ident = format_ident!("__arg_{}", param.ident);
        let ty = wire_ty_tokens(&param.ty, &quote!('_));
        quote! {
            let mut #ident: ::core::option::Option<#ty> = ::core::option::Option::None;
        }
    });
    let arms = params.iter().map(|param| {
        let ident = format_ident!("__arg_{}", param.ident);
        let field_id = param.field_id;
        let decode = decode_field_value(&param.ty);
        quote! {
            #field_id => {
                #ident = ::core::option::Option::Some(#decode);
            }
        }
    });
    let binds = params.iter().map(|param| {
        let opt_ident = format_ident!("__arg_{}", param.ident);
        let val_ident = format_ident!("__val_{}", param.ident);
        quote! {
            let #val_ident = match #opt_ident {
                ::core::option::Option::Some(__v) => __v,
                ::core::option::Option::None => {
                    return ::core::result::Result::Err(::ipc::wire::IpcError::MissingField);
                }
            };
        }
    });

    let has_cap = params.iter().any(|param| matches!(param.ty, WireTy::Cap));
    let in_handles_decl = if has_cap {
        quote!(let __in_handles = &__handles[..__len.handles];)
    } else {
        TokenStream::new()
    };
    let decode = quote! {
        #in_handles_decl
        #(#decls)*
        let mut __cursor = ::ipc::wire::FieldCursor::new(__body);
        while let ::core::option::Option::Some(__field) = __cursor.next_field()? {
            match __field.id {
                #(#arms)*
                // неизвестное поле пропущено курсором по len.
                _ => {}
            }
        }
        #(#binds)*
    };
    let call_args = params.iter().map(|param| {
        let val_ident = format_ident!("__val_{}", param.ident);
        quote!(#val_ident)
    });
    (decode, quote! { #(#call_args),* })
}

fn expand_dispatch(protocol: &Protocol) -> TokenStream {
    let vis = &protocol.vis;
    let trait_ident = &protocol.trait_ident;
    let service_ident = suffixed(trait_ident, "Service");
    let dispatch_ident = format_ident!("dispatch_{}", snake(trait_ident));

    let arms = protocol
        .operations
        .iter()
        .filter(|op| matches!(op.kind, Kind::Call | Kind::Cast))
        .map(expand_dispatch_arm);

    quote! {
        #vis fn #dispatch_ident<T: ::ipc::Transport, S: #service_ident>(
            srv: &mut S,
            transport: &T,
        ) -> ::core::result::Result<(), ::ipc::wire::IpcError> {
            let mut __bytes = [0u8; ::ipc::wire::MESSAGE_INLINE_MAX];
            let mut __handles = [0u32; ::ipc::wire::MESSAGE_MAX_HANDLES];
            let __len = transport.read_message(&mut __bytes, &mut __handles)?;
            let __frame = &__bytes[..__len.bytes];
            let __header = ::ipc::wire::Header::decode(__frame)?;
            let __body = &__frame[::ipc::wire::HEADER_SIZE..];
            let __txid = __header.txid;
            match __header.ordinal {
                #(#arms)*
                _ => {
                    // неизвестный strict-ordinal закрывает канал epitaph-кадром.
                    let mut __reply = ::ipc::wire::MessageBuf::<{ ::ipc::wire::MESSAGE_INLINE_MAX }>::new();
                    __reply.write_header(&::ipc::wire::Header::new(
                        __header.ordinal,
                        __txid,
                        ::ipc::wire::FLAG_EPITAPH,
                    ))?;
                    __reply.finish()?;
                    transport.write_message(__reply.as_bytes(), &[])?;
                    ::core::result::Result::Ok(())
                }
            }
        }
    }
}

fn expand_dispatch_arm(op: &Operation) -> TokenStream {
    let ordinal = op.ordinal;
    let method_ident = &op.ident;
    let (decode, call_args) = decode_params(&op.params);

    match op.kind {
        Kind::Cast => quote! {
            #ordinal => {
                #decode
                srv.#method_ident(#call_args);
                ::core::result::Result::Ok(())
            }
        },
        Kind::Call => {
            let ret = op.ret.as_ref().expect("two-way carries a return type");
            let (success, success_handles) =
                encode_return(&quote!(__ok), ret.ok(), &quote!(__reply));
            let body = if let Some(err) = ret.err() {
                let (encode_err, err_handles) =
                    encode_return(&quote!(__err), err, &quote!(__reply));
                quote! {
                    match srv.#method_ident(#call_args) {
                        ::core::result::Result::Ok(__ok) => {
                            let mut __reply = ::ipc::wire::MessageBuf::<{ ::ipc::wire::MESSAGE_INLINE_MAX }>::new();
                            __reply.write_header(&::ipc::wire::Header::new(
                                #ordinal,
                                __txid,
                                ::ipc::wire::FLAG_RESPONSE,
                            ))?;
                            #success
                            __reply.finish()?;
                            transport.write_message(__reply.as_bytes(), #success_handles)?;
                        }
                        ::core::result::Result::Err(__err) => {
                            let mut __reply = ::ipc::wire::MessageBuf::<{ ::ipc::wire::MESSAGE_INLINE_MAX }>::new();
                            __reply.write_header(&::ipc::wire::Header::new(
                                #ordinal,
                                __txid,
                                ::ipc::wire::FLAG_RESPONSE | ::ipc::wire::FLAG_DOMAIN_ERR,
                            ))?;
                            #encode_err
                            __reply.finish()?;
                            transport.write_message(__reply.as_bytes(), #err_handles)?;
                        }
                    }
                }
            } else {
                quote! {
                    let __ok = srv.#method_ident(#call_args);
                    let mut __reply = ::ipc::wire::MessageBuf::<{ ::ipc::wire::MESSAGE_INLINE_MAX }>::new();
                    __reply.write_header(&::ipc::wire::Header::new(
                        #ordinal,
                        __txid,
                        ::ipc::wire::FLAG_RESPONSE,
                    ))?;
                    #success
                    __reply.finish()?;
                    transport.write_message(__reply.as_bytes(), #success_handles)?;
                }
            };
            quote! {
                #ordinal => {
                    #decode
                    #body
                    ::core::result::Result::Ok(())
                }
            }
        }
        Kind::Event => TokenStream::new(),
    }
}

fn expand_events(protocol: &Protocol) -> TokenStream {
    let vis = &protocol.vis;
    let trait_ident = &protocol.trait_ident;
    let events_ident = suffixed(trait_ident, "Events");
    let sender_ident = suffixed(trait_ident, "EventSender");
    let dispatch_event_ident = format_ident!("dispatch_{}_event", snake(trait_ident));

    let events: Vec<&Operation> = protocol
        .operations
        .iter()
        .filter(|op| op.kind == Kind::Event)
        .collect();

    let handler_methods = events.iter().map(|op| {
        let method_ident = &op.ident;
        let params_sig = param_signature(&op.params);
        let docs = op_docs(op);
        quote! {
            #docs
            fn #method_ident(&mut self #params_sig);
        }
    });

    let dispatch_arms = events.iter().map(|op| {
        let ordinal = op.ordinal;
        let method_ident = &op.ident;
        let (decode, call_args) = decode_params(&op.params);
        quote! {
            #ordinal => {
                #decode
                handler.#method_ident(#call_args);
                ::core::result::Result::Ok(())
            }
        }
    });

    let sender_methods = events.iter().map(|op| {
        let method_ident = format_ident!("emit_{}", op.ident);
        let ordinal = op.ordinal;
        let (encode, encode_handles) = encode_params(&op.params);
        let params_sig = param_signature(&op.params);
        let docs = op_docs(op);
        quote! {
            #docs
            #vis fn #method_ident<T: ::ipc::Transport>(
                transport: &T
                #params_sig
            ) -> ::core::result::Result<(), ::ipc::wire::IpcError> {
                let mut __buf = ::ipc::wire::MessageBuf::<{ ::ipc::wire::MESSAGE_INLINE_MAX }>::new();
                __buf.write_header(&::ipc::wire::Header::new(#ordinal, 0u32, 0u16))?;
                #encode
                __buf.finish()?;
                transport.write_message(__buf.as_bytes(), #encode_handles)
            }
        }
    });

    quote! {
        #vis trait #events_ident {
            #(#handler_methods)*
        }

        #vis fn #dispatch_event_ident<T: ::ipc::Transport, H: #events_ident>(
            handler: &mut H,
            transport: &T,
        ) -> ::core::result::Result<(), ::ipc::wire::IpcError> {
            let mut __bytes = [0u8; ::ipc::wire::MESSAGE_INLINE_MAX];
            let mut __handles = [0u32; ::ipc::wire::MESSAGE_MAX_HANDLES];
            let __len = transport.read_message(&mut __bytes, &mut __handles)?;
            let __frame = &__bytes[..__len.bytes];
            let __header = ::ipc::wire::Header::decode(__frame)?;
            let __body = &__frame[::ipc::wire::HEADER_SIZE..];
            match __header.ordinal {
                #(#dispatch_arms)*
                _ => ::core::result::Result::Ok(()),
            }
        }

        #vis struct #sender_ident;

        impl #sender_ident {
            #(#sender_methods)*
        }
    }
}

fn expand_ordinal_module(protocol: &Protocol) -> TokenStream {
    let vis = &protocol.vis;
    let mod_ident = format_ident!("{}_ordinal", snake(&protocol.trait_ident));

    let consts = protocol.operations.iter().map(|op| {
        let const_ident = format_ident!("{}", op.ident.to_string().to_uppercase());
        let ordinal = op.ordinal;
        quote! {
            pub const #const_ident: u64 = #ordinal;
        }
    });

    let op_descs = protocol.operations.iter().map(operation_desc);

    quote! {
        #vis mod #mod_ident {
            #(#consts)*

            pub const DESC: ::ipc::schema::ProtocolDesc = ::ipc::schema::ProtocolDesc {
                operations: &[
                    #(#op_descs),*
                ],
            };
        }
    }
}

fn operation_desc(op: &Operation) -> TokenStream {
    let name = op.ident.to_string();
    let canonical = &op.canonical;
    let ordinal = op.ordinal;
    let kind = match op.kind {
        Kind::Call => quote!(::ipc::schema::Kind::Call),
        Kind::Cast => quote!(::ipc::schema::Kind::Cast),
        Kind::Event => quote!(::ipc::schema::Kind::Event),
    };
    let fields = op.params.iter().map(|param| {
        let field_id = param.field_id;
        let ty = wire_ty_desc(&param.ty);
        quote! {
            ::ipc::schema::FieldDesc { field_id: #field_id, field_type: #ty }
        }
    });

    quote! {
        ::ipc::schema::OperationDesc {
            name: #name,
            canonical: #canonical,
            ordinal: #ordinal,
            kind: #kind,
            fields: &[ #(#fields),* ],
        }
    }
}

fn snake(ident: &Ident) -> Ident {
    let raw = ident.to_string();
    let mut out = String::with_capacity(raw.len() + 4);
    for (i, ch) in raw.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    Ident::new(&out, Span::call_site())
}
