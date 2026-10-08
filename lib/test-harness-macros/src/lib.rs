//! Объявляет атрибут `#[test_harness::test]`, которым помечается функция интеграционного теста.
//!
//! Раскрытие оставляет функцию как есть и добавляет к ней регистрацию в наборе тестов: символ
//! записи, имя теста и его вид. Вид берётся из аргумента атрибута - `stress` либо `bench`.
//! Без аргумента тест считается обычным.
//!
//! Набор тестов держит и прогоняет `test-harness`. Вызывающий крейт должен подключать его под
//! именем `test_harness`, по которому к нему обращается раскрытие.

mod expand;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use syn::{Error, ItemFn, parse_macro_input};

#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attr = TokenStream2::from(attr);
    let item = parse_macro_input!(item as ItemFn);

    expand::expand_test(attr, &item)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}
