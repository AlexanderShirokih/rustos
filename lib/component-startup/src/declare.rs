/// Объявляет манифест компонента: протоколы портов и слоты.
///
/// Слот - имя, под которым компоненту при запуске выдаётся capability: сессия чужого
/// протокола, каталог, клиентская сторона чужого control-порта, регион образа, окно MMIO
/// или линия прерывания. Объявление слота называет вид capability и признак, по которому
/// сборка выбирает источник.
///
/// Макрос раскрывает объявление в структуру приёмника транзакции. Имя структуры взято из
/// объявления. Первые два поля структуры - владеющие хэндлы `control` и `service`. За ними
/// идёт поле на каждый объявленный слот под именем этого слота. Ассоциированная функция
/// `receive` принимает стартовую транзакцию по handle стартового порта и возвращает
/// [`ReceivedStartup`](crate::ReceivedStartup) с заполненной структурой и лог-сессией
/// компонента.
///
/// Запись манифеста попадает тем же раскрытием в секцию `.component.manifest` собранной
/// программы. По этой записи сборка строит слоты компонента.
///
/// Программа получает handle стартового порта при запуске (по соглашению аргументом точки
/// входа `_start`). Программа на Rust со стандартной библиотекой получает этот handle от
/// `component-startup-std`.
///
/// Объявление слота `rtc`, импортирующего внешний протокол `Rtc`, объявленный другим
/// компонентом. Под объявлением - приём транзакции по handle точки входа:
///
/// ```ignore
/// component_manifest! {
///     struct ClockStartup {
///         slots: { rtc: service(protocol = "Rtc") },
///     }
/// }
///
/// // Раскрытие даёт `struct ClockStartup { control, service, rtc }`.
/// fn run(startup: Handle) -> Result<(), StartupError> {
///     let received = ClockStartup::receive(OwnedHandle::from_handle(startup))?;
///     let rtc_session = received.resources.rtc;
///     Ok(())
/// }
/// ```
///
/// В ключе `exports` стоит имя протокола, который компонент обслуживает на своём
/// service-порту. В ключе `controls` - имя протокола control-порта компонента.
///
/// Без ключа `establishes_sessions` сессии service-порта выдаются клиентам помимо
/// компонента. С ключом `establishes_sessions: true` каждую сессию создаёт сам компонент по
/// запросу `ComponentSessions::open_session` на своём control-порту.
///
/// Объявление компонента, который обслуживает протокол `FileSystem` на своём service-порту и
/// сам создаёт сессии этого порта:
///
/// ```ignore
/// component_manifest! {
///     struct FileSystemStartup {
///         exports: "FileSystem",
///         establishes_sessions: true,
///         slots: {},
///     }
/// }
/// ```
///
/// Провайдер слота - компонент, который обслуживает названный протокол либо содержит нужный
/// каталог. Композиция поднимает одну программу несколькими компонентами под разными
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
#[macro_export]
macro_rules! component_manifest {
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            $(exports: $export:literal,)?
            $(controls: $control:literal,)?
            $(establishes_sessions: $establishes:literal,)?
            slots: { $($slot:ident: $kind:ident $(($($parameter:tt)*))?),* $(,)? } $(,)?
        }
    ) => {
        $(#[$attribute])*
        $visibility struct $name {
            /// Серверная сторона control-порта: его клиентскую сторону держит supervisor.
            pub control: $crate::OwnedHandle,
            /// Серверная сторона service-порта: его сессии supervisor минтит клиентам.
            pub service: $crate::OwnedHandle,
            $(pub $slot: $crate::OwnedHandle,)*
        }

        impl $name {
            /// Принимает стартовую транзакцию целиком по владеющему стартовому порту.
            $visibility fn receive(
                startup: $crate::OwnedHandle,
            ) -> ::core::result::Result<$crate::ReceivedStartup<Self>, $crate::StartupError> {
                const DECLARED: &[()] = &[$($crate::component_manifest!(@place $slot)),*];

                let mut set = $crate::receive_set(startup, DECLARED.len())?;
                $(let $slot = set.next_slot()?;)*
                ::core::result::Result::Ok($crate::ReceivedStartup {
                    log: set.log,
                    resources: Self {
                        control: set.control,
                        service: set.service,
                        $($slot,)*
                    },
                })
            }
        }

        $crate::component_abi::__component_manifest_record! {
            export = [$($export)?]
            control = [$($control)?]
            establishes = [$($establishes)?]
            slots { $($kind $(($($parameter)*))?),* }
        }
    };
    (@place $slot:ident) => { () };
}
