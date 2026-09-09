/// Объявляет манифест компонента: экспортируемые протоколы и слоты.
///
/// Слот - имя, под которым компоненту при запуске выдаётся capability: сессия чужого
/// экспорта, каталог, регион образа, окно MMIO или линия прерывания. Объявление слота
/// называет вид capability и признак, по которому сборка выбирает источник.
///
/// Макрос раскрывает объявление в структуру приёмника транзакции. Имя структуры взято из
/// объявления. Первое поле структуры - владеющий хэндл `startup`: серверная сторона порта
/// supervisor. Supervisor держит клиентскую сторону всю incarnation. За ним идёт поле на каждый
/// экспорт под именем его ключа и поле на каждый слот под именем этого слота.
/// Ассоциированная функция `receive` принимает стартовую транзакцию по handle стартового
/// порта и возвращает [`ReceivedStartup`](crate::ReceivedStartup) с заполненной структурой
/// и лог-сессией компонента.
///
/// Запись манифеста попадает тем же раскрытием в секцию `.component.manifest` собранной
/// программы. По этой записи сборка строит слоты компонента.
///
/// Программа получает handle стартового порта при запуске (по соглашению аргументом точки
/// входа `_start`). Программа на Rust со стандартной библиотекой получает этот handle от
/// `component-startup-std`.
///
/// Объявление слота `rtc`, импортирующего протокол `Rtc`, экспортированный другим
/// компонентом. Под объявлением - приём транзакции по handle точки входа:
///
/// ```ignore
/// component_manifest! {
///     struct ClockStartup {
///         slots: { rtc: service(protocol = "Rtc") },
///     }
/// }
///
/// // Раскрытие даёт `struct ClockStartup { startup, rtc }`.
/// fn run(startup: Handle) -> Result<(), StartupError> {
///     let received = ClockStartup::receive(OwnedHandle::from_handle(startup))?;
///     let rtc_session = received.resources.rtc;
///     Ok(())
/// }
/// ```
///
/// Чтобы экспортировать протокол другим компонентам, объявление называет его в ключе
/// `exports` парой "имя поля - протокол". Под именем протокола сборка образа находит
/// провайдера для чужого слота, а поле принимает серверную сторону порта этого экспорта.
/// Экспортов у компонента может быть несколько, и административный протокол объявляется
/// таким же экспортом, как и любой другой.
///
/// Без ключа `establishes_sessions` сессии экспортов выдаются клиентам помимо компонента.
/// С ключом `establishes_sessions: true` каждую сессию создаёт сам компонент по запросу
/// `ComponentStartup::open_session` на своём startup-порту.
///
/// Объявление компонента, который экспортирует `Namespace` и `NamespaceAdmin` на своих
/// портах и сам создаёт их сессии:
///
/// ```ignore
/// component_manifest! {
///     struct RouterStartup {
///         exports: { service: "Namespace", admin: "NamespaceAdmin" },
///         establishes_sessions: true,
///         slots: {},
///     }
/// }
///
/// // Раскрытие даёт `struct RouterStartup { startup, service, admin }`.
/// ```
///
/// Провайдер слота - компонент, который экспортирует названный протокол либо содержит
/// нужный каталог. Композиция поднимает одну программу несколькими компонентами под разными
/// именами экземпляров. Провайдер назван именем экземпляра.
///
/// Импорт протокола принимает необязательное `instance` - имя экземпляра-провайдера. Сборка
/// связывает импорт без имени с единственным провайдером этого протокола в композиции.
///
/// У слота-каталога протокол отсутствует. `instance` в таком слоте обязателен. Компонент
/// берёт у названного провайдера сессию, в корне которой лежит каталог компонента.
///
/// Объявление слота `files`, импортирующего протокол `FileSystem` у экземпляра `ramfs`, и
/// слота-каталога `area` у экземпляра `filesystem-imagefs`:
///
/// ```ignore
/// component_manifest! {
///     struct FileReaderStartup {
///         slots: {
///             files: service(protocol = "FileSystem", instance = "ramfs"),
///             area: directory(instance = "filesystem-imagefs"),
///         },
///     }
/// }
/// ```
///
/// Вид поля слота берётся из его объявления. Поле `service` принимает портовый транспорт,
/// а поле `mmio` - регион памяти:
///
/// ```no_run
/// use component_startup::{MemoryRegion, component_manifest};
/// use kernel_objects::PortTransport;
///
/// component_manifest! {
///     struct WindowStartup {
///         slots: {
///             rtc: service(protocol = "Rtc"),
///             window: mmio(compatible = ["arm,pl031"]),
///         },
///     }
/// }
///
/// fn use_fields(started: WindowStartup) {
///     let _ = PortTransport::client(&started.rtc);
///     let _: MemoryRegion = started.window;
/// }
///
/// fn main() {}
/// ```
///
/// Поле окна MMIO объектной операции линии прерывания не принимает:
///
/// ```compile_fail
/// use component_startup::component_manifest;
/// use syscall::Timeout;
///
/// component_manifest! {
///     struct WindowStartup {
///         slots: { window: mmio(compatible = ["arm,pl031"]) },
///     }
/// }
///
/// fn use_fields(started: WindowStartup) {
///     let _ = started.window.wait(Timeout::INFINITE);
/// }
///
/// fn main() {}
/// ```
#[macro_export]
macro_rules! component_manifest {
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            $(exports: { $($export:ident: $protocol:literal),* $(,)? },)?
            $(establishes_sessions: $establishes:literal,)?
            slots: { $($slot:ident: $kind:ident $(($($parameter:tt)*))?),* $(,)? } $(,)?
        }
    ) => {
        $(#[$attribute])*
        $visibility struct $name {
            /// Серверная сторона startup-порта: его клиентскую сторону supervisor держит
            /// всю incarnation.
            pub startup: $crate::Port,
            $($(pub $export: $crate::Port,)*)?
            $(pub $slot: $crate::OwnedHandle<
                $crate::component_manifest!(@slot_kind $kind)
            >,)*
        }

        impl $name {
            /// Принимает стартовую транзакцию целиком по владеющему стартовому порту.
            $visibility fn receive(
                startup: $crate::Port,
            ) -> ::core::result::Result<$crate::ReceivedStartup<Self>, $crate::StartupError> {
                const EXPORTS: &[()] = &[$($($crate::component_manifest!(@place $export)),*)?];
                const SLOTS: &[()] = &[$($crate::component_manifest!(@place $slot)),*];

                let mut set = $crate::receive_set(startup, EXPORTS.len(), SLOTS.len())?;
                $($(let $export = set.next_export()?;)*)?
                $(let $slot =
                    set.next_slot::<$crate::component_manifest!(@slot_kind $kind)>()?;)*
                ::core::result::Result::Ok($crate::ReceivedStartup {
                    log: set.log,
                    resources: Self {
                        startup: set.startup,
                        $($($export,)*)?
                        $($slot,)*
                    },
                })
            }
        }

        $crate::component_abi::__component_manifest_record! {
            exports = [$($($protocol),*)?]
            establishes = [$($establishes)?]
            slots { $($kind $(($($parameter)*))?),* }
        }
    };
    (@place $slot:ident) => { () };
    (@slot_kind userland) => { $crate::kind::MemoryRegion };
    (@slot_kind mmio) => { $crate::kind::MemoryRegion };
    (@slot_kind irq) => { $crate::kind::IrqLine };
    (@slot_kind service) => { $crate::kind::Port };
    (@slot_kind directory) => { $crate::kind::Port };
    (@slot_kind supervisor) => { $crate::kind::Port };
}
