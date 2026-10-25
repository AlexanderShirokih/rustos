//! Объявление IPC-контракта.
//!
//! Кодирование и декодирование полей задают трейты. [`IntoWireValue`](crate::IntoWireValue)
//! пишет запись поля, [`WireValue`](crate::WireValue) читает её, а
//! [`WireTyped`](crate::WireTyped) задаёт дескриптор типа. По дескрипторам полей
//! [`crate::schema`] проверяет бюджет кадра и пространство `field_id` каждой операции.

/// Срок ожидания получателя у клиента, которому вызывающий не задал срок.
pub const DEFAULT_OPERATION_TIMEOUT: crate::Timeout = crate::Timeout::from_secs(5);

/// Тип, представляющий один объявленный контракт.
///
/// [`crate::protocol!`] порождает такой тип для каждого объявления. [`Protocol::ID`] отдаёт
/// имя контракта из ключа `name`, и переименование Rust-пути к этому типу имя не меняет.
pub trait Protocol {
    /// Полное имя контракта.
    const ID: &'static str;

    /// Клиент контракта поверх канала `C`.
    type Client<C: crate::Channel>;

    /// Строит клиента поверх указанного канала.
    fn client<C: crate::Channel>(channel: C) -> Self::Client<C>;
}

/// Объявляет протокол с одним `Client`, одним `Service` и диспетчерами `dispatch` и `serve`.
///
/// `call fn` задаёт запрос с немедленным результатом обработчика, `call async fn` -
/// обработчик, который возвращает результат после своих ожиданий. `cast fn` и
/// `cast async fn` задают односторонние операции без ответа. Форма обработчика задаёт только
/// `Service` и диспетчеры, клиент у всех форм один.
///
/// ```ignore
/// ipc::protocol! {
///     pub mod clock {
///         name = "Clock";
///
///         call fn now() -> u64;
///         call async fn tick() -> u64;
///         cast fn arm(deadline_ns: u64);
///     }
/// }
/// ```
///
/// `Client::new(channel)` создаёт клиента, `with_wait` задаёт срок приёма и ответа,
/// `into_channel` возвращает канал. По умолчанию действует [`DEFAULT_OPERATION_TIMEOUT`].
/// Метод операции с ответом создаёт [`Call`](crate::Call), метод односторонней операции -
/// [`Send`](crate::Send). `.wait()` ждёт через синхронный [`Channel`](crate::Channel),
/// `.await` - через [`AsyncChannel`](crate::AsyncChannel). Способ ожидания не меняет ординал
/// или формат кадра. Владение параметрами описано у типов операций.
///
/// Ожидание `Call` возвращает прочитанный ответ либо транспортный
/// [`IpcError`](crate::wire::IpcError). Для ответа, объявленного буквально как `Result<T, E>`,
/// результат ожидания имеет тип `Result<T, CallError<E>>`.
/// Псевдоним `Result` и путь `core::result::Result` читаются как обычное значение ответа.
///
/// `Service<F>` получает `&self`, ресурсы кадра `frame: &F` и прочитанные параметры.
/// Объявленный асинхронный метод раскрывается в `async fn` без требования `Send` к future.
/// Диспетчер удерживает кадр и право ответа, пока метод ждёт. Невзятые capability закрываются
/// вместе с кадром. Исход доставки ответа и capability недоставленного ответа получает
/// обработчик `call(defer)` от [`Reply::send`](crate::Reply::send).
///
/// Если контракт содержит асинхронные методы, `dispatch` принимает
/// [`OwnedIncoming`](crate::OwnedIncoming)
/// и возвращает future. Контракт из синхронных методов получает синхронный `dispatch`
/// над [`Incoming`](crate::Incoming). Незнакомая операция и вызов без канала ответа
/// отбрасываются до обработчика, а неверный кадр возвращает ошибку разбора.
///
/// `serve(srv, incoming)` обслуживает кадр, заимствованный у буфера приёма. `srv` - клонируемый
/// владелец сервиса, например `Rc<S>`. Синхронную операцию `serve` разбирает на месте и
/// возвращает `Ok(None)`. Асинхронную операцию она копирует в
/// [`OwnedIncoming`](crate::OwnedIncoming) и возвращает `Ok(Some(future))`. Future владеет
/// копией кадра и клоном `srv`.
///
/// `call(defer) fn` и `call(defer) async fn` передают обработчику последним аргументом
/// [`Reply`](crate::Reply) вместо возвращаемого значения для делегирования ответа.
/// Разрушение долга без ответа даёт `PeerClosed`.
///
/// ```no_run
/// ipc::protocol! {
///     mod store {
///         name = "Store";
///         call fn now() -> u64;
///         call(defer) fn open() -> u32;
///     }
/// }
///
/// struct Server;
///
/// impl<F: ipc::ReceivedResources> store::Service<F> for Server {
///     fn now(&self, _frame: &F) -> u64 { 0 }
///
///     fn open(&self, _frame: &F, reply: store::open::Reply<F::Reply>) {
///         let _ = reply.send(7u32);
///     }
/// }
///
/// fn main() {}
/// ```
///
/// `own` перед параметром или типом ответа передаёт владение его capability. Клиент принимает
/// [`IntoWireValue`](crate::IntoWireValue) с объявленным типом `Read`, сервер читает
/// [`Received`](crate::Received). Параметр без `own` не может содержать capability.
/// Отказ односторонней отправки возвращает параметры через [`Undelivered`](crate::Undelivered):
/// один параметр своим значением, несколько - кортежем в порядке объявления.
/// Отказ кодирования или вызова закрывает capability параметров. Тайм-аут не подтверждает
/// отмену уже принятого сервером запроса.
///
/// `name` входит в каноническое имя операции. Переименование Rust-модуля идентичность
/// протокола не меняет. Модуль операции содержит `CANONICAL` и `ORDINAL`, а операция
/// делегирования - также псевдоним `Reply<C>`. Макрос проверяет бюджет кадра и пространство
/// `field_id` при компиляции. Превышение динамического размера даёт `FrameOverflow`.
#[macro_export]
macro_rules! protocol {
    (
        $(#[$attr:meta])*
        mod $name:ident {
            name = $proto:literal ;
            $($ops:tt)*
        }
    ) => {
        $crate::protocol!(@ops [[$(#[$attr])*] [pub(super)] $name $proto] [] $($ops)*);
    };

    (
        $(#[$attr:meta])*
        $vis:vis mod $name:ident {
            name = $proto:literal ;
            $($ops:tt)*
        }
    ) => {
        $crate::protocol!(@ops [[$(#[$attr])*] $vis [pub] $name $proto] [] $($ops)*);
    };

    // Каждое правило снимает одну операцию и передаёт её параметры в @params, который
    // возвращается сюда уже с разобранным списком. Модификатор операции с ответом разбирается
    // до захвата типа возврата, потому что захваченный `ty` повторному разбору не поддаётся.

    (@ops $ctx:tt [$($done:tt)*]) => {
        $crate::protocol!(@emit $ctx [$($done)*]);
    };

    (@ops $ctx:tt [$($done:tt)*]
        $(#[$doc:meta])* cast fn $op:ident ( $($params:tt)* ) ; $($rest:tt)*
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [cast $op [$(#[$doc])*] [()] [] [] []]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@ops $ctx:tt $done:tt
        $(#[$doc:meta])* call async fn $op:ident ( $($params:tt)* ) $($rest:tt)*
    ) => {
        $crate::protocol!(@call
            ctx = $ctx done = $done kind = call_async docs = [$(#[$doc])*] op = $op params = [$($params)*]
            defer = [] rest = [$($rest)*]
        );
    };

    (@ops $ctx:tt $done:tt
        $(#[$doc:meta])* call ( defer ) async fn $op:ident ( $($params:tt)* ) $($rest:tt)*
    ) => {
        $crate::protocol!(@call
            ctx = $ctx done = $done kind = call_async docs = [$(#[$doc])*] op = $op params = [$($params)*]
            defer = [defer] rest = [$($rest)*]
        );
    };

    (@ops $ctx:tt [$($done:tt)*]
        $(#[$doc:meta])* cast async fn $op:ident ( $($params:tt)* ) ; $($rest:tt)*
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [cast_async $op [$(#[$doc])*] [()] [] [] []]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@ops $ctx:tt $done:tt
        $(#[$doc:meta])* call fn $op:ident ( $($params:tt)* ) $($rest:tt)*
    ) => {
        $crate::protocol!(@call
            ctx = $ctx done = $done kind = call docs = [$(#[$doc])*] op = $op params = [$($params)*]
            defer = [] rest = [$($rest)*]
        );
    };

    (@ops $ctx:tt $done:tt
        $(#[$doc:meta])* call ( defer ) fn $op:ident ( $($params:tt)* ) $($rest:tt)*
    ) => {
        $crate::protocol!(@call
            ctx = $ctx done = $done kind = call docs = [$(#[$doc])*] op = $op params = [$($params)*]
            defer = [defer] rest = [$($rest)*]
        );
    };

    (@ops $ctx:tt $done:tt
        $(#[$doc:meta])* call ( $($mods:tt)* ) fn $op:ident $($rest:tt)*
    ) => {
        ::core::compile_error!(concat!(
            "operation `", stringify!($op), "` has an unknown modifier `", stringify!($($mods)*), "`"
        ));
    };

    (@call ctx = $ctx:tt done = [$($done:tt)*] kind = $kind:ident docs = $docs:tt op = $op:ident
        params = [$($params:tt)*] defer = $defer:tt rest = [-> own Result < $ok:ty , $err:ty > ; $($rest:tt)*]
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [$kind $op $docs [::core::result::Result<$ok, $err>] [own] $defer [$ok , $err]]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@call ctx = $ctx:tt done = [$($done:tt)*] kind = $kind:ident docs = $docs:tt op = $op:ident
        params = [$($params:tt)*] defer = $defer:tt rest = [-> Result < $ok:ty , $err:ty > ; $($rest:tt)*]
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [$kind $op $docs [::core::result::Result<$ok, $err>] [] $defer [$ok , $err]]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@call ctx = $ctx:tt done = [$($done:tt)*] kind = $kind:ident docs = $docs:tt op = $op:ident
        params = [$($params:tt)*] defer = $defer:tt rest = [-> own $ret:ty ; $($rest:tt)*]
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [$kind $op $docs [$ret] [own] $defer []]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@call ctx = $ctx:tt done = [$($done:tt)*] kind = $kind:ident docs = $docs:tt op = $op:ident
        params = [$($params:tt)*] defer = $defer:tt rest = [-> $ret:ty ; $($rest:tt)*]
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [$kind $op $docs [$ret] [] $defer []]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    // Тип клиентской сигнатуры и тип записи закрепляются за параметром. Владеющий параметр
    // получает обобщённый тип клиента из `names` и добавляется в список `owned`. По этому
    // списку клиент читает значения из неотправленного кадра.

    (@params ctx = $ctx:tt done = [$($done:tt)*] rest = [$($rest:tt)*]
        head = [$($head:tt)*] acc = [$($acc:tt)*] owned = [$($owned:tt)*] names = $names:tt
        id = $id:expr;
    ) => {
        $crate::protocol!(@ops $ctx [$($done)* { $($head)* [$($acc)*] [$($owned)*] }] $($rest)*);
    };

    (@params ctx = $ctx:tt done = $done:tt rest = $rest:tt head = $head:tt
        acc = [$($acc:tt)*] owned = [$($owned:tt)*] names = [$generic:ident $($names:ident)*]
        id = $id:expr;
        own $param:ident : $param_ty:ty $(, $($more:tt)*)?
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = $done rest = $rest head = $head
            acc = [$($acc)* { $id , $param , $generic , $param_ty , [own] }]
            owned = [$($owned)* { $id , $generic , $param , $param_ty }]
            names = [$($names)*]
            id = $id + 1u8;
            $($($more)*)?
        );
    };

    (@params ctx = $ctx:tt done = $done:tt rest = $rest:tt head = [$kind:ident $op:ident $($head:tt)*]
        acc = $acc:tt owned = $owned:tt names = [] id = $id:expr;
        own $($more:tt)*
    ) => {
        ::core::compile_error!(concat!(
            "operation `", stringify!($op),
            "` declares more owned parameters than its frame carries capabilities"
        ));
    };

    (@params ctx = $ctx:tt done = $done:tt rest = $rest:tt head = $head:tt
        acc = [$($acc:tt)*] owned = $owned:tt names = $names:tt id = $id:expr;
        $param:ident : $param_ty:ty $(, $($more:tt)*)?
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = $done rest = $rest head = $head
            acc = [$($acc)* { $id , $param , $param_ty , $param_ty , [] }]
            owned = $owned names = $names
            id = $id + 1u8;
            $($($more)*)?
        );
    };

    (@emit
        [[$(#[$attr:meta])*] $vis:vis [$server_vis:vis] $name:ident $proto:literal]
        [$( { $kind:ident $op:ident $docs:tt $ret:tt $ret_own:tt $defer:tt $flat:tt $params:tt $owned:tt } )*]
    ) => {
        $(#[$attr])*
        $vis mod $name {
            #[allow(unused_imports)]
            use super::*;

            $(
                /// Операция контракта: её ординал и разбор её кадра.
                pub mod $op {
                    #[allow(unused_imports)]
                    use super::super::*;

                    /// Каноническое имя операции: `<protocol>.<operation>`.
                    pub const CANONICAL: &::core::primitive::str =
                        concat!($proto, ".", stringify!($op));

                    /// Ординал операции: младшие 64 бита SHA-256 канонического имени.
                    pub const ORDINAL: u64 = $crate::ordinal_of(CANONICAL);

                    $crate::__protocol_budget! { $op $ret $params }

                    $crate::__protocol_reply_alias! { $defer $ret }
                }
            )*

            $crate::__protocol_client! {
                [$( { $kind $op $docs $ret $flat $params $owned } )*]
            }

            $crate::__protocol_server! {
                @select [$server_vis]
                [$( { $kind $op $docs $ret $ret_own $defer $params } )*]
                [$( { $kind $op $docs $ret $ret_own $defer $params } )*]
            }

            /// Тип контракта: его полное имя и клиент.
            pub struct Protocol;

            impl $crate::Protocol for Protocol {
                const ID: &'static ::core::primitive::str = $proto;

                type Client<C: $crate::Channel> = Client<C>;

                fn client<C: $crate::Channel>(channel: C) -> Client<C> {
                    Client::new(channel)
                }
            }
        }
    };
}

/// Const-проверки кадра операции. Тело укладывается в бюджет, а параметры - в пространство
/// `field_id`.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_budget {
    ($op:ident [$ret:ty]
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        const _: () = {
            let _ = <$ret as $crate::WireTyped>::WIRE_TYPE;
        };

        const _: () = ::core::assert!(
            $crate::schema::max_body_len(&[$( <$wire_ty as $crate::WireTyped>::WIRE_TYPE ),*])
                <= $crate::wire::BODY_MAX,
            concat!("frame of operation `", stringify!($op), "` exceeds the MESSAGE_INLINE_MAX budget")
        );

        const _: () = ::core::assert!(
            $crate::schema::type_fits_field_id_space(
                $crate::schema::WireType::Aggregate(&[
                    $( <$wire_ty as $crate::WireTyped>::WIRE_TYPE ),*
                ]),
                $crate::wire::FIELD_ID_MAX,
            ),
            concat!("operation `", stringify!($op), "` declares more fields than the field_id space carries")
        );

        $( $crate::__protocol_own_check!($op $param $own $wire_ty); )*
    };
}

/// Const-проверка параметра без `own`: его запись не содержит capability.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_own_check {
    ($op:ident $param:ident [own] $wire_ty:ty) => {};
    ($op:ident $param:ident [] $wire_ty:ty) => {
        const _: () = ::core::assert!(
            !$crate::schema::carries_capability(<$wire_ty as $crate::WireTyped>::WIRE_TYPE),
            concat!(
                "parameter `",
                stringify!($param),
                "` of `",
                stringify!($op),
                "` carries a capability and must be declared `own`"
            )
        );
    };
}

/// Тип, которым метод сервиса получает параметр. Параметр `own` приходит значением, которое
/// владеет его capability, прочий - объявленным типом.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_received {
    ([own] $frame:ident $wire_ty:ty) => { $crate::Received<$wire_ty, $frame> };
    ([] $frame:ident $wire_ty:ty) => { $wire_ty };
}

/// Долг ответа порождается только для явного делегирования.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_reply_alias {
    ([] $ret:tt) => {};
    ([defer] [$ret:ty]) => {
        pub type Reply<C> = $crate::Reply<$ret, C>;
    };
}

/// Серверный интерфейс и диспетчер, выбранный наличием асинхронных методов.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_server {
    (@select $vis:tt $all:tt [{ call_async $($op:tt)* } $($rest:tt)*]) => {
        $crate::__protocol_server!(@emit $vis [async] [&self] $all);
    };
    (@select $vis:tt $all:tt [{ cast_async $($op:tt)* } $($rest:tt)*]) => {
        $crate::__protocol_server!(@emit $vis [async] [&self] $all);
    };
    (@select $vis:tt $all:tt [{ $($op:tt)* } $($rest:tt)*]) => {
        $crate::__protocol_server!(@select $vis $all [$($rest)*]);
    };
    (@select $vis:tt $all:tt []) => {
        $crate::__protocol_server!(@emit $vis [] [&self] $all);
    };
    (@emit [$vis:vis] $mode:tt $receiver:tt
     [$( { $kind:ident $op:ident $docs:tt $ret:tt $ret_own:tt $defer:tt $params:tt } )*]) => {
        /// Серверная сторона контракта. Асинхронность метода задаёт его объявление.
        #[allow(async_fn_in_trait)]
        $vis trait Service<F: $crate::ReceivedResources> {
            $( $crate::__protocol_service_method! {
                $receiver $kind $op $docs $ret $ret_own $defer $params
            } )*
        }

        $crate::__protocol_dispatch! {
            $mode [$vis] [$( { $kind $op $ret $defer $params } )*]
        }
    };
}

/// Разбор кадра для единственного серверного интерфейса контракта.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_dispatch {
    ([] [$vis:vis] [$( { $kind:ident $op:ident $ret:tt $defer:tt $params:tt } )*]) => {
        /// Разбирает кадр и синхронно вызывает метод сервиса.
        $vis fn dispatch<F: $crate::ReceivedResources, S: Service<F>>(
            srv: &S,
            incoming: $crate::Incoming<'_, F>,
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            let $crate::Incoming { bytes, mut resources } = incoming;
            let $crate::wire::FrameParts { header, body } = $crate::wire::Header::split(bytes)?;
            $( if header.ordinal == $op::ORDINAL {
                return $crate::__protocol_serve! {
                    $kind $op $ret $defer $params [srv, &mut resources, body]
                };
            } )*
            ::core::result::Result::Ok(())
        }

        /// Обслуживает кадр на месте, как [`dispatch`], и возвращает `None`.
        $vis fn serve<F, O>(
            owner: &O,
            incoming: $crate::Incoming<'_, F>,
        ) -> ::core::result::Result<
            ::core::option::Option<
                ::core::future::Ready<::core::result::Result<(), $crate::wire::IpcError>>,
            >,
            $crate::wire::IpcError,
        >
        where
            F: $crate::ReceivedResources,
            O: $crate::ServiceOwner<Service: Service<F>>,
        {
            dispatch(owner.service(), incoming).map(|()| ::core::option::Option::None)
        }
    };
    ([async] [$vis:vis] [$( { $kind:ident $op:ident $ret:tt $defer:tt $params:tt } )*]) => {
        /// Обрабатывает владеющий кадр, удерживая его ресурсы во время ожидания метода.
        $vis fn dispatch<F: $crate::ReceivedResources, S: Service<F>>(
            srv: &S,
            incoming: $crate::OwnedIncoming<F>,
        ) -> impl ::core::future::Future<Output = ::core::result::Result<(), $crate::wire::IpcError>>
        {
            let ordinal = $crate::wire::Header::split(incoming.bytes()).map(|parts| parts.header.ordinal);
            serve_owned(srv, ordinal, incoming)
        }

        // Блок занимает свои захваты на месте: кадр хранится в future один раз.
        fn serve_owned<F, S, R>(
            srv: R,
            ordinal: ::core::result::Result<u64, $crate::wire::IpcError>,
            mut incoming: $crate::OwnedIncoming<F>,
        ) -> impl ::core::future::Future<Output = ::core::result::Result<(), $crate::wire::IpcError>>
               + use<F, S, R>
        where
            F: $crate::ReceivedResources,
            S: Service<F>,
            R: ::core::ops::Deref<Target = S>,
        {
            async move {
                let srv = &*srv;
                let ordinal = ordinal?;
                let $crate::Incoming { bytes, resources } = incoming.as_incoming();
                let body = bytes.get($crate::wire::HEADER_SIZE..).unwrap_or_default();
                $( if ordinal == $op::ORDINAL {
                    return $crate::__protocol_serve! {
                        $kind $op $ret $defer $params [srv, resources, body]
                    };
                } )*
                ::core::result::Result::Ok(())
            }
        }

        /// Обслуживает синхронную операцию на месте и возвращает `None`. Для асинхронной
        /// возвращает future, которая владеет копией кадра и клоном `owner`.
        $vis fn serve<F, O>(
            owner: &O,
            incoming: $crate::Incoming<'_, F>,
        ) -> ::core::result::Result<
            ::core::option::Option<
                impl ::core::future::Future<
                    Output = ::core::result::Result<(), $crate::wire::IpcError>,
                > + use<F, O>,
            >,
            $crate::wire::IpcError,
        >
        where
            F: $crate::ReceivedResources,
            O: $crate::ServiceOwner<Service: Service<F>>,
        {
            let $crate::Incoming { bytes, resources } = incoming;
            let parts = $crate::wire::Header::split(bytes)?;
            let mut deferred = false;
            $( $crate::__protocol_serve_now! {
                $kind $op $ret $defer $params [owner.service(), resources, parts, deferred]
            } )*
            if !deferred {
                return ::core::result::Result::Ok(::core::option::Option::None);
            }

            let incoming = $crate::OwnedIncoming::new($crate::Incoming { bytes, resources })?;
            ::core::result::Result::Ok(::core::option::Option::Some(serve_owned(
                $crate::__Owned(::core::clone::Clone::clone(owner)),
                ::core::result::Result::Ok(parts.header.ordinal),
                incoming,
            )))
        }
    };
}

/// Синхронная операция обслуживается на месте, асинхронная отмечается для копии кадра.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_serve_now {
    (call_async $op:ident $ret:tt $defer:tt $params:tt
     [$srv:expr, $resources:ident, $parts:ident, $deferred:ident]) => {
        if $parts.header.ordinal == $op::ORDINAL {
            $deferred = true;
        }
    };
    (cast_async $op:ident $ret:tt $defer:tt $params:tt
     [$srv:expr, $resources:ident, $parts:ident, $deferred:ident]) => {
        if $parts.header.ordinal == $op::ORDINAL {
            $deferred = true;
        }
    };
    ($kind:ident $op:ident $ret:tt $defer:tt $params:tt
     [$srv:expr, $resources:ident, $parts:ident, $deferred:ident]) => {
        if $parts.header.ordinal == $op::ORDINAL {
            let mut $resources = $resources;
            let served: ::core::result::Result<(), $crate::wire::IpcError> = $crate::__protocol_serve! {
                $kind $op $ret $defer $params [$srv, &mut $resources, $parts.body]
            };
            return served.map(|()| ::core::option::Option::None);
        }
    };
}

/// Ожидание добавляется только у объявленного async-метода.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_invoke {
    ([] $call:expr) => {
        $call
    };
    ([async] $call:expr) => {
        $call.await
    };
}

/// Разбор и ответ операции общие для синхронного и асинхронного вызова.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_serve {
    (@reply [$ret:ty] [$channel:expr, $ordinal:expr, $value:expr]) => {
        $crate::Reply::<$ret, F::Reply>::__new($channel, $ordinal).__send($value)
    };

    (call $($op:tt)*) => { $crate::__protocol_serve!(@call [] $($op)*) };
    (call_async $($op:tt)*) => { $crate::__protocol_serve!(@call [async] $($op)*) };
    (cast $($op:tt)*) => { $crate::__protocol_serve!(@cast [] $($op)*) };
    (cast_async $($op:tt)*) => { $crate::__protocol_serve!(@cast [async] $($op)*) };
    (@cast $mode:tt $op:ident $ret:tt []
     [$( { $id:expr , $param:ident , $sig:ty , $wire:ty , $own:tt } )*]
     [$srv:expr, $resources:expr, $body:expr]) => {{
        let frame = $resources;
        $crate::__protocol_decode_params!($body, frame, [$( { $id , $param , $wire } )*]);
        $crate::__protocol_invoke!($mode $srv.$op(&*frame $(, $param)*));
        ::core::result::Result::Ok(())
    }};
    (@call $mode:tt $op:ident [$ret:ty] []
     [$( { $id:expr , $param:ident , $sig:ty , $wire:ty , $own:tt } )*]
     [$srv:expr, $resources:expr, $body:expr]) => {{
        let frame = $resources;
        match $crate::ReceivedResources::take_reply(frame) {
            ::core::option::Option::Some(channel) => {
                $crate::__protocol_decode_params!($body, frame, [$( { $id , $param , $wire } )*]);
                let value = $crate::__protocol_invoke!($mode $srv.$op(&*frame $(, $param)*));
                $crate::__protocol_serve!(@reply [$ret] [channel, $op::ORDINAL, value])
            }
            ::core::option::Option::None => ::core::result::Result::Ok(()),
        }
    }};
    (@call $mode:tt $op:ident [$ret:ty] [defer]
     [$( { $id:expr , $param:ident , $sig:ty , $wire:ty , $own:tt } )*]
     [$srv:expr, $resources:expr, $body:expr]) => {{
        let frame = $resources;
        match $crate::ReceivedResources::take_reply(frame) {
            ::core::option::Option::Some(channel) => {
                $crate::__protocol_decode_params!($body, frame, [$( { $id , $param , $wire } )*]);
                $crate::__protocol_invoke!(
                    $mode $srv.$op(&*frame $(, $param)*, $op::Reply::__new(channel, $op::ORDINAL))
                );
                ::core::result::Result::Ok(())
            }
            ::core::option::Option::None => ::core::result::Result::Ok(()),
        }
    }};
}

/// Разбирает тело кадра в объявленные параметры операции.
///
/// Раскрывается в объявления с именами параметров: следом их видно как обычные значения.
/// Capability параметров разбор забирает из ресурсов кадра `$frame`. Отсутствие объявленного
/// поля - `MissingField`, неизвестное поле пропускается курсором. Второе поле тела с тем же
/// `field_id` курсор отвергает как `FieldOrder`.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_decode_params {
    ($body:expr, $frame:expr, [$( { $field_id:expr , $param:ident , $wire_ty:ty } )*]) => {
        $( let mut $param = ::core::option::Option::None; )*
        let mut cursor = $crate::wire::FieldCursor::new($body);
        while let ::core::option::Option::Some(field) = cursor.next_field()? {
            $(
                if field.id == $field_id {
                    $param = ::core::option::Option::Some(field.data);
                }
            )*
        }
        $(
            let ::core::option::Option::Some($param) = $param else {
                return ::core::result::Result::Err($crate::wire::IpcError::MissingField);
            };
        )*
        $(
            let $param =
                <$wire_ty as $crate::WireValue<'_, _>>::from_field($param, &mut *$frame)?;
        )*
    };
}

/// Записи параметров операции в кадр `frame`.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_write_params {
    ([$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        move |frame: &mut $crate::wire::MessageBuf<_>| {
            $( $crate::IntoWireValue::write_into($param, frame, $field_id)?; )*
            ::core::result::Result::Ok(())
        }
    };
}

/// Единственный клиент контракта. Вид метода задаёт объявление операции.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_client {
    ([$( { $($method:tt)* } )*]) => {
        /// Клиент контракта поверх канала.
        pub struct Client<C: $crate::Channel> {
            channel: C,
            wait: $crate::Timeout,
        }

        impl<C: $crate::Channel> Client<C> {
            /// Создаёт клиента со сроком по умолчанию
            /// [`DEFAULT_OPERATION_TIMEOUT`]($crate::DEFAULT_OPERATION_TIMEOUT).
            pub fn new(channel: C) -> Self {
                Self { channel, wait: $crate::DEFAULT_OPERATION_TIMEOUT }
            }

            /// Задаёт срок приёма и ответа для операций клиента.
            #[must_use]
            pub fn with_wait(mut self, wait: $crate::Timeout) -> Self {
                self.wait = wait;
                self
            }

            /// Отдаёт канал обратно.
            pub fn into_channel(self) -> C { self.channel }

            $( $crate::__protocol_client_method! {
                $($method)*
            } )*
        }
    };
}

/// Метод единственного клиента: [`Call`](crate::Call) у операции с ответом,
/// [`Send`](crate::Send) у односторонней.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_client_method {
    (call $($method:tt)*) => {
        $crate::__protocol_client_method!(@call $($method)*);
    };
    (call_async $($method:tt)*) => {
        $crate::__protocol_client_method!(@call $($method)*);
    };
    (cast $($method:tt)*) => {
        $crate::__protocol_client_method!(@cast $($method)*);
    };
    (cast_async $($method:tt)*) => {
        $crate::__protocol_client_method!(@cast $($method)*);
    };

    (@call $op:ident [$(#[$doc:meta])*] [$read:ty] [$ok:ty , $err:ty]
     [$( { $id:expr , $param:ident , $sig:ty , $wire:ty , $own:tt } )*]
     [$( { $owned_id:expr , $generic:ident , $owned:ident , $owned_ty:ty } )*]) => {
        $(#[$doc])*
        pub fn $op<$($generic: $crate::IntoWireValue<C::Resources, Read = $owned_ty>),*>(
            &self $(, $param: $sig)*
        ) -> $crate::Call<'_, C, $crate::Received<$ok, C::Resources>,
                         $crate::CallError<$crate::Received<$err, C::Resources>>> {
            $crate::Call::__new(
                &self.channel, $op::ORDINAL, self.wait,
                $crate::__protocol_write_params!([$( { $id , $param , $sig , $wire , $own } )*]),
                |ordinal, reply| $crate::__protocol_outcome::<$ok, $err, _>(ordinal, reply),
            )
        }
    };
    (@call $op:ident [$(#[$doc:meta])*] [$read:ty] []
     [$( { $id:expr , $param:ident , $sig:ty , $wire:ty , $own:tt } )*]
     [$( { $owned_id:expr , $generic:ident , $owned:ident , $owned_ty:ty } )*]) => {
        $(#[$doc])*
        pub fn $op<$($generic: $crate::IntoWireValue<C::Resources, Read = $owned_ty>),*>(
            &self $(, $param: $sig)*
        ) -> $crate::Call<'_, C, $crate::Received<$read, C::Resources>> {
            $crate::Call::__new(
                &self.channel, $op::ORDINAL, self.wait,
                $crate::__protocol_write_params!([$( { $id , $param , $sig , $wire , $own } )*]),
                |ordinal, reply| $crate::__protocol_answer::<$read, _>(ordinal, reply),
            )
        }
    };

    // Отказ отдаёт единственный владеющий параметр сам, а несколько - кортежем в порядке
    // объявления.
    (@cast $op:ident $docs:tt $ret:tt $flat:tt $params:tt []) => {
        $crate::__protocol_client_method! {
            @send $op $docs $params [] [()] [()] [$crate::wire::IpcError]
        }
    };
    (@cast $op:ident $docs:tt $ret:tt $flat:tt $params:tt
     [{ $id:expr , $generic:ident , $owned:ident , $wire:ty }]) => {
        $crate::__protocol_client_method! {
            @send $op $docs $params [{ $id , $generic , $owned , $wire }]
            [$crate::Received<$wire, C::Resources>] [$owned]
            [$crate::Undelivered<$crate::Received<$wire, C::Resources>>]
        }
    };
    (@cast $op:ident $docs:tt $ret:tt $flat:tt $params:tt
     [$( { $id:expr , $generic:ident , $owned:ident , $wire:ty } )+]) => {
        $crate::__protocol_client_method! {
            @send $op $docs $params [$( { $id , $generic , $owned , $wire } )+]
            [($($crate::Received<$wire, C::Resources>,)+)] [($($owned,)+)]
            [$crate::Undelivered<($($crate::Received<$wire, C::Resources>,)+)>]
        }
    };
    (@send $op:ident [$(#[$doc:meta])*]
     [$( { $id:expr , $param:ident , $sig:ty , $wire:ty , $own:tt } )*]
     [$( { $owned_id:expr , $generic:ident , $owned:ident , $owned_ty:ty } )*]
     [$value:ty] [$refused:expr] [$error:ty]) => {
        $(#[$doc])*
        pub fn $op<$($generic: $crate::IntoWireValue<C::Resources, Read = $owned_ty>),*>(
            &self $(, $param: $sig)*
        ) -> $crate::Send<'_, C, $value, $error> {
            $crate::Send::__new(
                &self.channel, $op::ORDINAL, self.wait,
                $crate::__protocol_write_params!([$( { $id , $param , $sig , $wire , $own } )*]),
                |body, frame| {
                    $crate::__protocol_decode_params!(body, frame, [$( { $owned_id , $owned , $owned_ty } )*]);
                    ::core::result::Result::Ok($refused)
                },
            )
        }
    };
}

/// Сигнатура метода единственного серверного трейта.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_service_method {
    (@call [$($async:tt)*] [$($receiver:tt)*] $op:ident [$(#[$doc:meta])*] [$ret:ty] [] []
     [$( { $id:expr , $param:ident , $sig:ty , $wire:ty , $own:tt } )*]) => {
        $(#[$doc])*
        $($async)* fn $op($($receiver)*, frame: &F $(, $param: $crate::__protocol_received!($own F $wire))*) -> $ret;
    };
    (@call [$($async:tt)*] [$($receiver:tt)*] $op:ident [$(#[$doc:meta])*] [$ret:ty] [own] []
     [$( { $id:expr , $param:ident , $sig:ty , $wire:ty , $own:tt } )*]) => {
        $(#[$doc])*
        $($async)* fn $op($($receiver)*, frame: &F $(, $param: $crate::__protocol_received!($own F $wire))*)
            -> impl $crate::IntoWireValue<F, Read = $ret>;
    };
    (@call [$($async:tt)*] [$($receiver:tt)*] $op:ident [$(#[$doc:meta])*] $ret:tt $ret_own:tt [defer]
     [$( { $id:expr , $param:ident , $sig:ty , $wire:ty , $own:tt } )*]) => {
        $(#[$doc])*
        $($async)* fn $op(
            $($receiver)*, frame: &F $(, $param: $crate::__protocol_received!($own F $wire))*,
            reply: $op::Reply<F::Reply>,
        );
    };
    ($receiver:tt call $($op:tt)*) => {
        $crate::__protocol_service_method!(@call [] $receiver $($op)*);
    };
    ($receiver:tt call_async $($op:tt)*) => {
        $crate::__protocol_service_method!(@call [async] $receiver $($op)*);
    };
    ($receiver:tt cast $op:ident $docs:tt $ret:tt $own:tt $defer:tt $params:tt) => {
        $crate::__protocol_service_method!(@call [] $receiver $op $docs [()] [] [] $params);
    };
    ($receiver:tt cast_async $op:ident $docs:tt $ret:tt $own:tt $defer:tt $params:tt) => {
        $crate::__protocol_service_method!(@call [async] $receiver $op $docs [()] [] [] $params);
    };

}

#[inline]
pub(crate) fn build_frame<R: crate::ReceivedResources>(
    frame: &mut crate::wire::MessageBuf<R>,
    header: crate::wire::Header,
    write: impl FnOnce(&mut crate::wire::MessageBuf<R>) -> Result<(), crate::wire::IpcError>,
) -> Result<(), crate::wire::IpcError> {
    frame.write_header(&header)?;
    write(frame)?;
    frame.finish()
}

pub(crate) fn read_answer<'b, V: crate::WireValue<'b, R>, R: crate::ReceivedResources>(
    body: &'b [u8],
    resources: &mut R,
) -> Result<V::Owned, crate::wire::IpcError> {
    V::from_field(answer_field(body)?, resources)
}

fn answer_field(body: &[u8]) -> Result<&[u8], crate::wire::IpcError> {
    let mut cursor = crate::wire::FieldCursor::new(body);
    let mut value = None;
    while let Some(field) = cursor.next_field()? {
        if field.id == crate::wire::FIELD_ID_MIN {
            value = Some(field.data);
        }
    }
    value.ok_or(crate::wire::IpcError::MissingField)
}

fn answer_body(ordinal: u64, bytes: &[u8]) -> Result<&[u8], crate::wire::IpcError> {
    let crate::wire::FrameParts { header, body } = crate::wire::Header::split(bytes)?;
    if header.ordinal != ordinal {
        return Err(crate::wire::IpcError::InvalidValue);
    }
    Ok(body)
}

/// Разбирает кадр ответа операции `ordinal`: значение ответа занимает поле `FIELD_ID_MIN`.
/// Ответ другой операции - `InvalidValue`, пустое тело - `MissingField`. Capability, которых
/// не назвало значение, закрываются вместе с ресурсами кадра.
#[doc(hidden)]
pub fn __protocol_answer<'b, V: crate::WireValue<'b, R>, R: crate::ReceivedResources>(
    ordinal: u64,
    reply: crate::Incoming<'b, R>,
) -> Result<V::Owned, crate::wire::IpcError> {
    let crate::Incoming {
        bytes,
        mut resources,
    } = reply;
    read_answer::<V, R>(answer_body(ordinal, bytes)?, &mut resources)
}

/// Разбирает кадр ответа `Result<T, E>` операции `ordinal` сразу в итог метода клиента.
/// Значение успеха и отказа строится на месте возврата, без промежуточного `Result`. Отказы
/// разбора те же, что у [`__protocol_answer`].
#[doc(hidden)]
pub fn __protocol_outcome<'b, T, E, R>(
    ordinal: u64,
    reply: crate::Incoming<'b, R>,
) -> Result<T::Owned, crate::CallError<E::Owned>>
where
    T: crate::WireValue<'b, R>,
    E: crate::WireValue<'b, R>,
    R: crate::ReceivedResources,
{
    let crate::Incoming {
        bytes,
        mut resources,
    } = reply;
    let field = crate::codec::outcome_field(answer_field(answer_body(ordinal, bytes)?)?)?;
    if field.id == crate::codec::RESULT_OK_FIELD {
        T::from_field(field.data, &mut resources).map_err(crate::CallError::Ipc)
    } else {
        Err(crate::CallError::Remote(E::from_field(
            field.data,
            &mut resources,
        )?))
    }
}
