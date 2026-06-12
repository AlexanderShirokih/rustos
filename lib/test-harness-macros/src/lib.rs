use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Error, ItemFn, ReturnType, parse_macro_input};

#[proc_macro_attribute]
pub fn kernel_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = TokenStream2::from(attr);
    let item = parse_macro_input!(item as ItemFn);

    expand_kernel_test(attr, &item)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand_kernel_test(attr: TokenStream2, item: &ItemFn) -> syn::Result<TokenStream2> {
    if !attr.is_empty() {
        return Err(Error::new_spanned(
            attr,
            "#[kernel_test] does not accept arguments yet",
        ));
    }

    validate_signature(item)?;

    let ident = &item.sig.ident;
    let registration_ident = format_ident!("__kernel_test_{ident}");
    let test_name = ident.to_string();

    Ok(quote! {
        #item

        ::kernel_tests::register_test!(#registration_ident, #test_name, #ident);
    })
}

fn validate_signature(item: &ItemFn) -> syn::Result<()> {
    let sig = &item.sig;

    if let Some(constness) = &sig.constness {
        return Err(Error::new_spanned(
            constness,
            "#[kernel_test] does not support const functions",
        ));
    }

    if let Some(asyncness) = &sig.asyncness {
        return Err(Error::new_spanned(
            asyncness,
            "#[kernel_test] does not support async functions",
        ));
    }

    if let Some(unsafety) = &sig.unsafety {
        return Err(Error::new_spanned(
            unsafety,
            "#[kernel_test] does not support unsafe functions",
        ));
    }

    if sig.abi.is_some() {
        return Err(Error::new_spanned(
            &sig.abi,
            "#[kernel_test] does not support extern functions",
        ));
    }

    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &sig.generics,
            "#[kernel_test] does not support generic functions",
        ));
    }

    if !sig.inputs.is_empty() {
        return Err(Error::new_spanned(
            &sig.inputs,
            "#[kernel_test] functions must not take arguments",
        ));
    }

    if !matches!(sig.output, ReturnType::Default) {
        return Err(Error::new_spanned(
            &sig.output,
            "#[kernel_test] functions must not return a value",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use quote::quote;
    use syn::parse_quote;

    use super::{TokenStream2, expand_kernel_test};

    #[test]
    fn expands_plain_function_into_register_test_call() {
        let item: syn::ItemFn = parse_quote! {
            fn smoke() {}
        };

        let expanded = expand_kernel_test(TokenStream2::new(), &item).unwrap();
        let rendered = expanded.to_string();

        assert!(rendered.contains("fn smoke"));
        assert!(rendered.contains(":: kernel_tests :: register_test !"));
        assert!(rendered.contains("\"smoke\""));
        assert!(rendered.contains("__kernel_test_smoke"));
    }

    #[test]
    fn rejects_arguments_for_now() {
        let item: syn::ItemFn = parse_quote! {
            fn smoke() {}
        };

        let error = expand_kernel_test(quote!(timeout = 2), &item).unwrap_err();
        assert!(error.to_string().contains("does not accept arguments yet"));
    }

    #[test]
    fn rejects_non_plain_function_signatures() {
        let item: syn::ItemFn = parse_quote! {
            fn smoke(arg: usize) -> usize {
                arg
            }
        };

        let error = expand_kernel_test(TokenStream2::new(), &item).unwrap_err();
        assert!(error.to_string().contains("must not take arguments"));
    }
}
