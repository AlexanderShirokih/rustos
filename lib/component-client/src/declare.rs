/// Объявляет манифест компонента - обслуживаемые протоколы и именованные слоты вместе с тем,
/// чем композиция их наполняет: строки совместимости у устройства, имя протокола у импорта.
/// Порождает владеющий приёмник стартовой транзакции и запись манифеста в секции программы.
///
/// Объявление одно на обе формы программы. Freestanding-программа вызывает `receive` с handle
/// своей точки входа, программа со стандартной библиотекой - `component_client_std::receive`
/// с тем же приёмником.
///
/// Ключ `exports` называет протокол service-порта, ключ `controls` - протокол control-порта.
/// Порты разные, и объявляются они порознь.
///
/// Ключ `establishes_sessions: true` объявляет, что компонент сам создаёт сессии своего
/// service-порта. Композиция запрашивает их операцией `ComponentSessions::open_session` на
/// его control-порту вместо минта значка.
///
/// Импорт протокола принимает необязательное `instance` - имя экземпляра-провайдера. Импорт
/// без имени связывается с единственным провайдером протокола в композиции.
///
/// Слот-каталог называет `instance` обязательно. Протокол у него отсутствует, а сессию
/// своего каталога компонент берёт у названного провайдера.
///
/// ```ignore
/// component_client::component_manifest! {
///     pub struct ImageFileSystemStartup {
///         exports: "FileSystem",
///         establishes_sessions: true,
///         slots: { image: userland },
///     }
/// }
///
/// component_client::component_manifest! {
///     pub struct DemoStartup {
///         slots: {
///             files: service(protocol = "FileSystem", instance = "ramfs"),
///             mounts: control(protocol = "NamespaceAdmin"),
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
