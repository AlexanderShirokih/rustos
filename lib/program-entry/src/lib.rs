//! Атрибуты точки входа freestanding-программы: `#[program_entry::component]` и
//! `#[program_entry::process]`.
//!
//! Атрибут стоит на функции программы и оставляет её как есть. Раскрытие добавляет кучу
//! процесса, символ `_start` и обработчик паники. Функция возвращает тип, реализующий
//! `runtime::Termination`, и код выхода следует ему через `runtime::finish`. Паника в любом потоке передаётся `runtime::panic` и завершает процесс
//! кодом `runtime::FAILED_EXIT_CODE`, как и запуск без стартового handle.
//!
//! Раскрытие обращается к крейту `runtime`, у компонента - ещё к `component_startup`, у
//! процесса - к `kernel_objects`. Программа должна подключать эти крейты под их именами.

mod expand;

use expand::Program;
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use syn::{ItemFn, parse_macro_input};

/// Объявляет точку входа компонента, чей стартовый handle - стартовая сессия.
///
/// Точка входа ставит неявные импорты компонента и вызывает функцию со значением требований,
/// которое строит `component_startup::Requirements::from_parent`. Handle стартовой сессии
/// остаётся открытым до завершения процесса. Функция принимает один параметр, тип которого
/// реализует `Requirements`.
///
/// ```rust
/// #[program_entry::component]
/// fn run(requirements: RtcStartup) -> Result<(), Error> {
///     let service = Rtc::probe(requirements.platform)?;
///     serve(requirements.parent, service)
/// }
/// ```
#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);

    expand::expand_entry(Program::Component, TokenStream2::from(attr), &item)
        .unwrap_or_else(|error| expand::rejected(&item, error))
        .into()
}

/// Объявляет точку входа программы, которой стартовый handle передал запустивший её процесс.
///
/// Функция принимает единственным параметром владельца стартового handle
/// `kernel_objects::OwnedHandle<K>` либо ссылку на него. Вид `K` задаёт тип параметра, и
/// стартовый handle должен указывать на объект этого вида. Владельца, переданного по ссылке,
/// точка входа держит до завершения процесса.
///
/// ```rust
/// #[program_entry::process]
/// fn run(kernel: &Port) -> Result<(), Failed> {
///     let client = bootstrap::Client::new(PortTransport::new(kernel.as_raw()));
///     client.acquire_userland_image().map(drop).map_err(|_| Failed)
/// }
/// ```
#[proc_macro_attribute]
pub fn process(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);

    expand::expand_entry(Program::Process, TokenStream2::from(attr), &item)
        .unwrap_or_else(|error| expand::rejected(&item, error))
        .into()
}
