use proc_macro2::{Ident, Span, TokenStream as TokenStream2};
use quote::{quote, quote_spanned};
use syn::{Error, FnArg, ItemFn, ReturnType, Type, spanned::Spanned};

#[derive(Clone, Copy)]
pub(crate) enum Program {
    Component,
    Process,
}

impl Program {
    fn attribute(self) -> &'static str {
        match self {
            Self::Component => "#[program_entry::component]",
            Self::Process => "#[program_entry::process]",
        }
    }
}

pub(crate) fn expand_entry(
    program: Program,
    attr: TokenStream2,
    item: &ItemFn,
) -> syn::Result<TokenStream2> {
    let attribute = program.attribute();
    if !attr.is_empty() {
        return Err(Error::new_spanned(
            attr,
            format!("{attribute} takes no arguments"),
        ));
    }

    let (parameter, output) = validate_signature(program, item)?;
    let run = &item.sig.ident;
    // Имена раскрытия не видны коду программы и не заслоняют её функцию.
    let start = Ident::new("start", Span::mixed_site());
    let parent = Ident::new("parent", Span::mixed_site());
    // Владелец стартового handle находится в кадре `_start` до завершения процесса.
    let (prepare, argument) = match program {
        Program::Component => (
            quote! {
                let #parent = #start.assume_kind::<::runtime::kind::Port>();
                ::runtime::start_component(#parent.as_raw());
            },
            quote! {
                <#parameter as ::component_startup::Requirements>::from_parent(#parent.as_raw())
            },
        ),
        Program::Process => (
            quote! {
                let #start = ::kernel_objects::OwnedHandle::from(#start).assume_kind();
            },
            if matches!(parameter, Type::Reference(_)) {
                quote!(&#start)
            } else {
                quote!(#start)
            },
        ),
    };
    // Тип возврата без `runtime::Termination` компилятор показывает на самом типе.
    let finish = quote_spanned!(output.span()=> ::runtime::finish);

    let support = support();

    // SAFETY: `start` порождённой `_start` - значение `x0` точки входа, и точка входа
    // исполняется один раз.
    Ok(quote! {
        #item

        #support

        #[allow(unsafe_code)]
        #[unsafe(no_mangle)]
        extern "C" fn _start(#start: u32) -> ! {
            unsafe { ::runtime::start(#start) };
            let Some(#start) = ::runtime::take_bootstrap() else {
                ::runtime::exit(::runtime::FAILED_EXIT_CODE)
            };
            #prepare
            #finish(#run(#argument))
        }
    })
}

pub(crate) fn rejected(item: &ItemFn, error: Error) -> TokenStream2 {
    // Куча и обработчик паники оставляют ошибку сигнатуры единственной ошибкой сборки.
    let error = error.into_compile_error();
    let support = support();
    quote! {
        #item

        #error

        #support
    }
}

fn support() -> TokenStream2 {
    quote! {
        ::runtime::process_heap!();

        #[panic_handler]
        fn program_entry_panic(info: &::core::panic::PanicInfo<'_>) -> ! {
            ::runtime::panic(info)
        }
    }
}

fn validate_signature(program: Program, item: &ItemFn) -> syn::Result<(&Type, &Type)> {
    let attribute = program.attribute();
    let sig = &item.sig;

    if let Some(constness) = &sig.constness {
        return Err(Error::new_spanned(
            constness,
            format!("{attribute} does not support const functions"),
        ));
    }

    if let Some(asyncness) = &sig.asyncness {
        return Err(Error::new_spanned(
            asyncness,
            format!("{attribute} does not support async functions"),
        ));
    }

    if let Some(unsafety) = &sig.unsafety {
        return Err(Error::new_spanned(
            unsafety,
            format!("{attribute} does not support unsafe functions"),
        ));
    }

    if sig.abi.is_some() {
        return Err(Error::new_spanned(
            &sig.abi,
            format!("{attribute} does not support extern functions"),
        ));
    }

    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &sig.generics,
            format!("{attribute} does not support generic functions"),
        ));
    }

    let mut inputs = sig.inputs.iter();
    let (Some(input), None) = (inputs.next(), inputs.next()) else {
        return Err(Error::new_spanned(
            sig,
            format!("{attribute} functions take exactly one parameter"),
        ));
    };
    let FnArg::Typed(input) = input else {
        return Err(Error::new_spanned(
            input,
            format!("{attribute} functions take no receiver"),
        ));
    };
    let parameter = input.ty.as_ref();
    if let Type::Reference(reference) = parameter
        && reference.mutability.is_some()
    {
        return Err(Error::new_spanned(
            parameter,
            format!("{attribute} functions take no exclusive reference"),
        ));
    }

    let output = match &sig.output {
        ReturnType::Type(_, output) if !matches!(output.as_ref(), Type::Never(_)) => {
            output.as_ref()
        }
        _ => {
            return Err(Error::new_spanned(
                &sig.output,
                format!("{attribute} functions return `runtime::Termination`"),
            ));
        }
    };

    Ok((parameter, output))
}

#[cfg(test)]
mod tests {
    use quote::quote;
    use syn::{ItemFn, parse_quote};

    use super::{Program, TokenStream2, expand_entry, rejected};

    fn expanded(program: Program, item: &ItemFn) -> String {
        expand_entry(program, TokenStream2::new(), item)
            .expect("signature is accepted")
            .to_string()
    }

    fn rejection(program: Program, item: &ItemFn) -> String {
        expand_entry(program, TokenStream2::new(), item)
            .expect_err("signature is rejected")
            .to_string()
    }

    #[test]
    fn a_component_entry_imports_and_builds_its_requirements() {
        let item: ItemFn = parse_quote! {
            fn run(requirements: RtcStartup) -> Result<(), Error> {
                serve(requirements)
            }
        };

        let rendered = expanded(Program::Component, &item);

        assert!(rendered.contains("fn run (requirements : RtcStartup)"));
        assert!(rendered.contains(":: runtime :: process_heap ! ()"));
        assert!(rendered.contains("# [unsafe (no_mangle)] extern \"C\" fn _start"));
        assert!(rendered.contains(":: runtime :: start_component (parent . as_raw ())"));
        assert!(
            rendered
                .contains("< RtcStartup as :: component_startup :: Requirements > :: from_parent")
        );
        assert!(rendered.contains(":: runtime :: finish (run ("));
        assert!(rendered.contains("# [panic_handler]"));
    }

    #[test]
    fn a_process_entry_passes_its_start_port_without_imports() {
        let item: ItemFn = parse_quote! {
            fn run(kernel: &Port) -> Result<(), Failed> {
                Ok(())
            }
        };

        let rendered = expanded(Program::Process, &item);

        assert!(
            rendered.contains(":: kernel_objects :: OwnedHandle :: from (start) . assume_kind ()")
        );
        assert!(rendered.contains(":: runtime :: finish (run (& start))"));
        assert!(!rendered.contains("start_component"));
    }

    #[test]
    fn a_process_entry_hands_over_an_owned_start_handle() {
        let item: ItemFn = parse_quote! {
            fn run(report: MemoryRegion) -> ExitCode {
                ExitCode::from(0)
            }
        };

        let rendered = expanded(Program::Process, &item);

        assert!(rendered.contains(":: runtime :: finish (run (start))"));
    }

    #[test]
    fn a_rejected_signature_leaves_only_its_own_error() {
        let item: ItemFn = parse_quote! {
            fn run() -> Result<(), Failed> {
                Ok(())
            }
        };
        let error = expand_entry(Program::Component, TokenStream2::new(), &item).unwrap_err();

        let rendered = rejected(&item, error).to_string();

        assert!(rendered.contains("fn run ()"));
        assert!(rendered.contains("compile_error !"));
        assert!(rendered.contains(":: runtime :: process_heap ! ()"));
        assert!(rendered.contains("# [panic_handler]"));
        assert!(!rendered.contains("_start"));
    }

    #[test]
    fn a_result_alias_is_accepted() {
        let item: ItemFn = parse_quote! {
            fn run(parent: Handle<kind::Port>) -> crate::Result<()> {
                Ok(())
            }
        };

        expanded(Program::Component, &item);
    }

    #[test]
    fn rejects_attribute_arguments() {
        let item: ItemFn = parse_quote! {
            fn run(parent: Handle<kind::Port>) -> Result<(), Error> {
                Ok(())
            }
        };

        let error = expand_entry(Program::Component, quote!(imports = false), &item).unwrap_err();
        assert!(error.to_string().contains("takes no arguments"));
    }

    #[test]
    fn rejects_a_parameter_count_other_than_one() {
        let none: ItemFn = parse_quote! {
            fn run() -> Result<(), Error> {
                Ok(())
            }
        };
        let two: ItemFn = parse_quote! {
            fn run(parent: Handle<kind::Port>, extra: u32) -> Result<(), Error> {
                Ok(())
            }
        };

        assert!(rejection(Program::Component, &none).contains("exactly one parameter"));
        assert!(rejection(Program::Process, &two).contains("exactly one parameter"));
    }

    #[test]
    fn rejects_a_missing_or_never_return_type() {
        let unit: ItemFn = parse_quote! {
            fn run(parent: Handle<kind::Port>) {}
        };
        let never: ItemFn = parse_quote! {
            fn run(parent: Handle<kind::Port>) -> ! {
                loop {}
            }
        };

        assert!(rejection(Program::Component, &unit).contains("return `runtime::Termination`"));
        assert!(rejection(Program::Component, &never).contains("return `runtime::Termination`"));
    }

    #[test]
    fn rejects_async_and_generic_functions() {
        let asynchronous: ItemFn = parse_quote! {
            async fn run(parent: Handle<kind::Port>) -> Result<(), Error> {
                Ok(())
            }
        };
        let generic: ItemFn = parse_quote! {
            fn run<R: Requirements>(requirements: R) -> Result<(), Error> {
                Ok(())
            }
        };

        assert!(rejection(Program::Component, &asynchronous).contains("async functions"));
        assert!(rejection(Program::Component, &generic).contains("generic functions"));
    }

    #[test]
    fn rejects_an_exclusive_reference_parameter() {
        let exclusive: ItemFn = parse_quote! {
            fn run(kernel: &mut Port) -> Result<(), Failed> {
                Ok(())
            }
        };

        assert!(rejection(Program::Process, &exclusive).contains("exclusive reference"));
    }
}
