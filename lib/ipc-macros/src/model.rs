//! Парсинг и валидация trait-контракта в проверенную модель.
//!
//! Из `#[protocol]`-trait строится модель: имя-неймспейс, операции с видом,
//! типами параметров (только поддержанное множество), позиционными
//! `field_id` и вычисленными ordinal.

use ipc_schema::Kind;
use proc_macro2::Span;
use syn::{
    Attribute, Error, Expr, ExprLit, FnArg, GenericArgument, Ident, ItemTrait, Lit, Meta,
    PathArguments, ReturnType, TraitItem, TraitItemFn, Type, spanned::Spanned,
};

use crate::ordinal::{canonical_name, ordinal_of};

const BODY_MAX: usize = 242;

const FIELD_OVERHEAD: usize = 2;

const FIELD_DATA_MAX: usize = BODY_MAX - FIELD_OVERHEAD - 1;

/// Граница bounded-типа.
pub struct Bound {
    /// Исходная форма для const-generic позиции `Str<#expr>`: литерал без
    /// скобок либо `{ EXPR }` с ними (скобки там обязательны).
    pub expr: Expr,
    /// Развёрнутая форма для позиции аргумента (дескриптор, бюджет-assert),
    /// где компилятор считает обрамляющие скобки лишними.
    pub value: Expr,
    /// Значение, если граница - целочисленный литерал (для проверки бюджета).
    pub lit: Option<usize>,
}

/// Тип параметра/значения в макро-модели; bounded-типы несут выражение границы.
pub enum WireTy {
    Uint(u8),
    Int(u8),
    Bool,
    Str(Box<Bound>),
    Bytes(Box<Bound>),
    Cap,
}

/// Верхняя оценка длины `data` поля при максимальном значении; `None` для
/// не-литеральной границы (бюджет проверяется генерируемым const-assert).
fn max_data_len(ty: &WireTy) -> Option<usize> {
    match ty {
        WireTy::Uint(width) | WireTy::Int(width) => Some(*width as usize),
        WireTy::Bool | WireTy::Cap => Some(1),
        WireTy::Str(bound) | WireTy::Bytes(bound) => bound.lit,
    }
}

/// Owned-тип допустим в возврате two-way.
fn is_owned(ty: &WireTy) -> bool {
    matches!(
        ty,
        WireTy::Uint(_) | WireTy::Int(_) | WireTy::Bool | WireTy::Cap
    )
}

/// Параметр операции: имя, позиционный `field_id`, тип.
pub struct Param {
    pub ident: Ident,
    pub field_id: u8,
    pub ty: WireTy,
}

/// Тип возврата two-way операции.
pub enum RetType {
    /// Только тип успеха `T`.
    Value(WireTy),
    /// `Result<T, E>`: успех `T` и доменная ошибка `E`.
    Result { ok: WireTy, err: WireTy },
}

impl RetType {
    /// Тип успеха `T`.
    pub fn ok(&self) -> &WireTy {
        match self {
            RetType::Value(ok) | RetType::Result { ok, .. } => ok,
        }
    }

    /// Тип доменной ошибки `E`, если объявлен.
    pub fn err(&self) -> Option<&WireTy> {
        match self {
            RetType::Value(_) => None,
            RetType::Result { err, .. } => Some(err),
        }
    }
}

/// Операция протокола после валидации.
pub struct Operation {
    pub kind: Kind,
    pub ident: Ident,
    pub canonical: String,
    pub ordinal: u64,
    pub params: Vec<Param>,
    /// Тип возврата; задан только для two-way (`Kind::Call`).
    pub ret: Option<RetType>,
    /// doc-атрибуты операции, пробрасываемые в сгенерированные методы.
    pub docs: Vec<Attribute>,
    /// Per-call тайм-аут ожидания ответа в нс из `#[call(timeout_ns = N)]`.
    /// `None` - использовать дефолт клиента (`wait_ns`). Только для
    /// [`Kind::Call`].
    pub timeout_ns: Option<u64>,
}

/// Транспортная плоскость протокола: `Port` (двунаправленный канал) допускает
/// все операции, `Ring` (однонаправленное SPSC-кольцо) - только `#[cast]`/`#[event]`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TransportPlane {
    Port,
    Ring,
}

/// Контракт после валидации: операции с вычисленными ordinal.
pub struct Protocol {
    pub vis: syn::Visibility,
    pub trait_ident: Ident,
    pub operations: Vec<Operation>,
    /// Дефолтный тайм-аут клиента (`wait_ns`) из
    /// `#[protocol(timeout_ns = N)]`. `None` - `u64::MAX` (бессрочно).
    pub default_timeout_ns: Option<u64>,
    /// Транспортная плоскость из `#[protocol(transport = "...")]`.
    /// По умолчанию [`TransportPlane::Port`].
    pub plane: TransportPlane,
}

struct ProtocolArgs {
    name: Option<String>,
    timeout_ns: Option<u64>,
    plane: TransportPlane,
}

fn parse_protocol_args(
    meta: &syn::punctuated::Punctuated<Meta, syn::Token![,]>,
) -> syn::Result<ProtocolArgs> {
    let mut name: Option<String> = None;
    let mut timeout_ns: Option<u64> = None;
    let mut plane = TransportPlane::Port;
    for item in meta {
        let Meta::NameValue(nv) = item else {
            return Err(Error::new(item.span(), "expected name = \"...\""));
        };
        if nv.path.is_ident("name") {
            name = Some(parse_str_lit(&nv.value)?);
        } else if nv.path.is_ident("timeout_ns") {
            timeout_ns = Some(parse_u64_lit(&nv.value)?);
        } else if nv.path.is_ident("transport") {
            plane = parse_transport_plane(&nv.value)?;
        } else {
            return Err(Error::new(nv.path.span(), "unknown protocol argument"));
        }
    }
    Ok(ProtocolArgs {
        name,
        timeout_ns,
        plane,
    })
}

/// Разбирает `transport = "port" | "ring"` в [`TransportPlane`]; любое иное
/// значение - ошибка со span литерала.
fn parse_transport_plane(expr: &Expr) -> syn::Result<TransportPlane> {
    match parse_str_lit(expr)?.as_str() {
        "port" => Ok(TransportPlane::Port),
        "ring" => Ok(TransportPlane::Ring),
        _ => Err(Error::new(
            expr.span(),
            "transport must be \"port\" or \"ring\"",
        )),
    }
}

fn parse_str_lit(expr: &Expr) -> syn::Result<String> {
    if let Expr::Lit(ExprLit {
        lit: Lit::Str(s), ..
    }) = expr
    {
        Ok(s.value())
    } else {
        Err(Error::new(expr.span(), "expected string literal"))
    }
}

fn parse_u64_lit(expr: &Expr) -> syn::Result<u64> {
    if let Expr::Lit(ExprLit {
        lit: Lit::Int(int), ..
    }) = expr
    {
        int.base10_parse::<u64>()
    } else {
        Err(Error::new(expr.span(), "expected integer literal (ns)"))
    }
}

/// Строит проверенную модель из распарсенного trait и аргументов атрибута.
pub fn build(
    args: &syn::punctuated::Punctuated<Meta, syn::Token![,]>,
    item: &ItemTrait,
) -> syn::Result<Protocol> {
    let parsed = parse_protocol_args(args)?;
    let trait_ident = item.ident.clone();
    let name = parsed.name.unwrap_or_else(|| trait_ident.to_string());

    if !item.generics.params.is_empty() {
        return Err(Error::new(
            item.generics.span(),
            "#[protocol] does not support a generic trait",
        ));
    }
    if item.supertraits.iter().next().is_some() {
        return Err(Error::new(
            item.supertraits.span(),
            "#[protocol] does not support a supertrait",
        ));
    }

    let mut operations = Vec::new();
    for trait_item in &item.items {
        match trait_item {
            TraitItem::Fn(method) => {
                operations.push(build_operation(&name, method)?);
            }
            other => {
                return Err(Error::new(
                    other.span(),
                    "#[protocol] body holds only operation declarations",
                ));
            }
        }
    }

    check_ordinal_collisions(&operations)?;

    if parsed.plane == TransportPlane::Ring
        && let Some(call) = operations.iter().find(|op| op.kind == Kind::Call)
    {
        return Err(Error::new(
            call.ident.span(),
            "ring transport plane does not allow #[call] (a ring has no reply channel); use #[cast]",
        ));
    }

    Ok(Protocol {
        vis: item.vis.clone(),
        trait_ident,
        operations,
        default_timeout_ns: parsed.timeout_ns,
        plane: parsed.plane,
    })
}

fn build_operation(protocol: &str, method: &TraitItemFn) -> syn::Result<Operation> {
    let (kind, timeout_ns) = classify_kind(method)?;
    let ident = method.sig.ident.clone();
    let canonical = canonical_name(protocol, &ident.to_string());
    let ordinal = ordinal_of(&canonical);

    if method.default.is_some() {
        return Err(Error::new(
            method.span(),
            "#[protocol] operation has no default body",
        ));
    }
    if !method.sig.generics.params.is_empty() {
        return Err(Error::new(
            method.sig.generics.span(),
            "#[protocol] operation cannot be generic",
        ));
    }

    let params = collect_params(kind, method)?;
    let ret = resolve_return(kind, method)?;
    let docs = method
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("doc"))
        .cloned()
        .collect();

    Ok(Operation {
        kind,
        ident,
        canonical,
        ordinal,
        params,
        ret,
        docs,
        timeout_ns,
    })
}

/// Определяет вид операции и (для `#[call(timeout_ns = N)]`) её per-call
/// тайм-аут. `#[cast]`/`#[event]` аргументов не несут.
fn classify_kind(method: &TraitItemFn) -> syn::Result<(Kind, Option<u64>)> {
    let mut found: Option<Kind> = None;
    let mut timeout_ns: Option<u64> = None;
    for attr in &method.attrs {
        let kind = if attr.path().is_ident("call") {
            Kind::Call
        } else if attr.path().is_ident("cast") {
            Kind::Cast
        } else if attr.path().is_ident("event") {
            Kind::Event
        } else if attr.path().is_ident("doc") {
            // doc-комментарий операции вид не задаёт; пробрасывается кодогеном.
            continue;
        } else {
            return Err(Error::new(
                attr.span(),
                "operation is marked only with #[call], #[cast] or #[event]",
            ));
        };
        match (kind, &attr.meta) {
            // `#[call(timeout_ns = N)]` несёт per-call тайм-аут.
            (Kind::Call, Meta::List(_)) => timeout_ns = Some(parse_call_timeout(attr)?),
            // `#[call]`/`#[cast]`/`#[event]` без аргументов.
            (_, Meta::Path(_)) => {}
            _ => {
                return Err(Error::new(
                    attr.span(),
                    "only #[call] takes arguments: #[call(timeout_ns = N)]",
                ));
            }
        }
        set_kind(&mut found, kind, attr.span())?;
    }
    let kind = found.ok_or_else(|| {
        Error::new(
            method.span(),
            "operation requires #[call], #[cast] or #[event]",
        )
    })?;
    Ok((kind, timeout_ns))
}

/// Разбирает `#[call(timeout_ns = N)]`: единственный аргумент - целочисленный
/// `timeout_ns` (нс).
fn parse_call_timeout(attr: &Attribute) -> syn::Result<u64> {
    let mut timeout_ns: Option<u64> = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("timeout_ns") {
            let lit: syn::LitInt = meta.value()?.parse()?;
            timeout_ns = Some(lit.base10_parse::<u64>()?);
            Ok(())
        } else {
            Err(meta.error("unknown #[call] argument; expected timeout_ns = N"))
        }
    })?;
    timeout_ns.ok_or_else(|| Error::new(attr.span(), "#[call(...)] requires timeout_ns = N"))
}

fn set_kind(slot: &mut Option<Kind>, kind: Kind, span: Span) -> syn::Result<()> {
    if slot.is_some() {
        return Err(Error::new(
            span,
            "operation is marked with more than one kind",
        ));
    }
    *slot = Some(kind);
    Ok(())
}

fn collect_params(kind: Kind, method: &TraitItemFn) -> syn::Result<Vec<Param>> {
    let mut params = Vec::new();
    let mut next_field_id: u8 = 1;
    let mut body_used = 1usize; // терминатор тела

    for arg in &method.sig.inputs {
        match arg {
            FnArg::Receiver(receiver) => {
                if kind == Kind::Event {
                    return Err(Error::new(
                        receiver.span(),
                        "#[event] is declared without a self receiver",
                    ));
                }
            }
            FnArg::Typed(pat_type) => {
                let ident = pat_ident(&pat_type.pat)?;
                let ty = resolve_wire_ty(&pat_type.ty)?;
                // Литеральная граница проверяется на месте; не-литеральную
                // (выводимую из const) проверит генерируемый const-assert.
                if let Some(data) = max_data_len(&ty) {
                    body_used += FIELD_OVERHEAD + data;
                    if body_used > BODY_MAX {
                        return Err(Error::new(
                            pat_type.span(),
                            "operation frame exceeds the MESSAGE_INLINE_MAX budget",
                        ));
                    }
                }
                params.push(Param {
                    ident,
                    field_id: next_field_id,
                    ty,
                });
                next_field_id = next_field_id
                    .checked_add(1)
                    .ok_or_else(|| Error::new(pat_type.span(), "too many parameters"))?;
            }
        }
    }

    if kind != Kind::Event && !has_self_receiver(method) {
        return Err(Error::new(
            method.sig.span(),
            "a call/cast operation is declared with a &self receiver",
        ));
    }

    Ok(params)
}

fn has_self_receiver(method: &TraitItemFn) -> bool {
    method
        .sig
        .inputs
        .iter()
        .any(|arg| matches!(arg, FnArg::Receiver(_)))
}

fn pat_ident(pat: &syn::Pat) -> syn::Result<Ident> {
    if let syn::Pat::Ident(pat_ident) = pat {
        Ok(pat_ident.ident.clone())
    } else {
        Err(Error::new(
            pat.span(),
            "operation parameter is a plain name",
        ))
    }
}

fn resolve_return(kind: Kind, method: &TraitItemFn) -> syn::Result<Option<RetType>> {
    match kind {
        Kind::Cast | Kind::Event => {
            if !matches!(method.sig.output, ReturnType::Default) {
                return Err(Error::new(
                    method.sig.output.span(),
                    "one-way operation has no return",
                ));
            }
            Ok(None)
        }
        Kind::Call => {
            let ReturnType::Type(_, ty) = &method.sig.output else {
                return Err(Error::new(
                    method.sig.span(),
                    "two-way #[call] declares success type T or Result<T, E>",
                ));
            };
            Ok(Some(resolve_ret_ty(ty)?))
        }
    }
}

fn resolve_ret_ty(ty: &Type) -> syn::Result<RetType> {
    if let Some((ok_ty, err_ty)) = result_args(ty) {
        let ok = resolve_wire_ty(ok_ty)?;
        let err = resolve_wire_ty(err_ty)?;
        require_owned_return(&ok, ok_ty)?;
        require_owned_return(&err, err_ty)?;
        Ok(RetType::Result { ok, err })
    } else {
        let ok = resolve_wire_ty(ty)?;
        require_owned_return(&ok, ty)?;
        Ok(RetType::Value(ok))
    }
}

fn require_owned_return(wire: &WireTy, ty: &Type) -> syn::Result<()> {
    if is_owned(wire) {
        Ok(())
    } else {
        Err(Error::new(
            ty.span(),
            "two-way return must be an owned type: u8..u64, i8..i64, bool",
        ))
    }
}

fn result_args(ty: &Type) -> Option<(&Type, &Type)> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Result" {
        return None;
    }
    let PathArguments::AngleBracketed(generics) = &segment.arguments else {
        return None;
    };
    let mut types = generics.args.iter().filter_map(|arg| match arg {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let ok = types.next()?;
    let err = types.next()?;
    Some((ok, err))
}

fn resolve_wire_ty(ty: &Type) -> syn::Result<WireTy> {
    let Type::Path(type_path) = ty else {
        return Err(unsupported_type(ty));
    };
    let segment = type_path
        .path
        .segments
        .last()
        .ok_or_else(|| unsupported_type(ty))?;
    let name = segment.ident.to_string();

    match name.as_str() {
        "u8" => Ok(WireTy::Uint(1)),
        "u16" => Ok(WireTy::Uint(2)),
        "u32" => Ok(WireTy::Uint(4)),
        "u64" => Ok(WireTy::Uint(8)),
        "i8" => Ok(WireTy::Int(1)),
        "i16" => Ok(WireTy::Int(2)),
        "i32" => Ok(WireTy::Int(4)),
        "i64" => Ok(WireTy::Int(8)),
        "bool" => Ok(WireTy::Bool),
        "Str" => Ok(WireTy::Str(Box::new(bound_arg(segment, ty)?))),
        "Bytes" => Ok(WireTy::Bytes(Box::new(bound_arg(segment, ty)?))),
        "FieldStr" => Ok(WireTy::Str(Box::new(field_data_max_bound()))),
        "FieldBytes" => Ok(WireTy::Bytes(Box::new(field_data_max_bound()))),
        "Cap" => Ok(WireTy::Cap),
        _ => Err(unsupported_type(ty)),
    }
}

fn field_data_max_bound() -> Bound {
    let n = FIELD_DATA_MAX;
    let int_lit = syn::LitInt::new(&n.to_string(), Span::call_site());
    let expr = Expr::Lit(ExprLit { attrs: vec![], lit: Lit::Int(int_lit) });
    Bound { expr: expr.clone(), value: expr, lit: Some(n) }
}

/// Извлекает границу bounded-типа: любой const-аргумент `Str<N>` / `Str<{ N }>`.
/// Литерал запоминается отдельно для compile-time проверки бюджета.
fn bound_arg(segment: &syn::PathSegment, ty: &Type) -> syn::Result<Bound> {
    let PathArguments::AngleBracketed(generics) = &segment.arguments else {
        return Err(Error::new(
            ty.span(),
            "Str/Bytes require a length bound: Str<N>, Bytes<N>",
        ));
    };
    for arg in &generics.args {
        if let GenericArgument::Const(expr) = arg {
            let expr = expr.clone();
            let value = unwrap_const_block(expr.clone());
            let lit = if let Expr::Lit(ExprLit {
                lit: Lit::Int(int), ..
            }) = &value
            {
                Some(int.base10_parse::<usize>()?)
            } else {
                None
            };
            return Ok(Bound { expr, value, lit });
        }
    }
    Err(Error::new(
        ty.span(),
        "Str/Bytes bound is a const length: Str<N> or Str<{ EXPR }>",
    ))
}

/// Снимает скобки `{ PATH }`/`{ LIT }`: syn требует их, чтобы распознать
/// const-аргумент, но компилятор считает их лишними в const-generic позиции.
fn unwrap_const_block(expr: Expr) -> Expr {
    if let Expr::Block(block) = &expr
        && let [syn::Stmt::Expr(inner, None)] = block.block.stmts.as_slice()
        && matches!(inner, Expr::Path(_) | Expr::Lit(_))
    {
        return inner.clone();
    }
    expr
}

fn unsupported_type(ty: &Type) -> Error {
    Error::new(
        ty.span(),
        "unsupported type: allowed u8..u64, i8..i64, bool, Str<N>, Bytes<N>",
    )
}

fn check_ordinal_collisions(operations: &[Operation]) -> syn::Result<()> {
    for (i, left) in operations.iter().enumerate() {
        for right in &operations[i + 1..] {
            if left.ordinal == right.ordinal {
                return Err(Error::new(
                    right.ident.span(),
                    "ordinal collision: operations yield the same hash",
                ));
            }
        }
    }
    Ok(())
}
