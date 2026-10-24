//! Объявление IPC-контракта.
//!
//! Сам макрос в типы полей не заглядывает - он только раскладывает объявление по местам. Что
//! с каким типом делать, решают трейты. [`IntoWireValue`](crate::IntoWireValue) пишет запись
//! поля, [`WireValue`](crate::WireValue) читает её, а [`WireTyped`](crate::WireTyped) задаёт
//! дескриптор типа. По дескрипторам полей const-функции [`crate::schema`] проверяют бюджет кадра
//! и пространство `field_id` каждой операции.

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

/// Объявляет IPC-контракт. Объявление даёт контракту имя и перечисляет операции.
///
/// Вид операции задаёт её ключевое слово. `call fn` - запрос с ответом, `cast fn` -
/// односторонняя операция от клиента к серверу.
///
/// У операции `call` две серверные формы, и форму выбирает объявление. Без модификатора метод
/// сервиса возвращает значение ответа, и ответ отправляет приёмник кадра. Модификатор
/// `call(defer)` передаёт методу последним аргументом долг ответа [`Reply`](crate::Reply), и
/// метод значения не возвращает. Долг отвечают вызовом `send` сразу либо после возврата метода,
/// а разрушение долга без ответа даёт вызывателю `PeerClosed`. Клиентский метод, ординал и
/// кадры операции от формы не зависят.
///
/// Слово `own` перед именем параметра либо перед типом ответа передаёт получателю владение
/// значением. Помеченный параметр принимает любое значение [`IntoWireValue`](crate::IntoWireValue)
/// с записью `Read` - типом из объявления, и тем же значением сервер отдаёт помеченный ответ. Вид
/// capability приходит из типа записи. Запись переносит владельцев capability в кадр. Успех
/// отправки отдаёт capability получателю, отказ возвращает кадр отправителю. Клиентский метод
/// `cast` с владеющими параметрами возвращает при отказе [`Undelivered`](crate::Undelivered),
/// которое читает параметры в принятом виде: один параметр - сам, несколько - кортежем в порядке
/// объявления. Отказ записи кадра и отказ `call` закрывают владеющие параметры. `Timeout` вызова
/// приходит и после того, как сервер принял кадр, и тогда capability уже у сервера.
///
/// Получатель читает параметр `own` и любой ответ значением [`Received`](crate::Received),
/// которое владеет capability записи. Параметр без `own` приходит значением объявленного типа
/// и capability не содержит, а объявление с такой capability отвергает const-ассерт.
///
/// Заголовок объявления `name = "Proto"` называет протокол, и это имя входит в каноническое имя
/// каждой операции.
///
/// Раскрытие порождает модуль с именем и видимостью из объявления. Модуль встаёт на место
/// объявления и берёт окружающую область через `use super::*` - оттуда и приходят типы полей.
/// Внутри модуля лежат:
///
/// - `struct Client<C: Channel>` с методом на каждую операцию. Метод `call` возвращает
///   прочитанный ответ, метод `cast` - пустой успех. Ответ, объявленный буквально как
///   `Result<T, E>`, метод `call` отдаёт одним уровнем: успех `T` либо
///   [`CallError<E>`](crate::CallError). Ответ через псевдоним или путь `core::result::Result`
///   метод отдаёт прочитанным ответом, как любой другой тип. Ассоциированная функция
///   `Client::new(channel)` строит клиента, `with_wait` задаёт срок ожидания получателя,
///   `into_channel` отдаёт канал обратно;
/// - `struct AsyncClient<C: AsyncChannel>` с теми же конструкторами, сроком по умолчанию и
///   методами операций. Метод возвращает future с результатом метода `Client`;
/// - `trait Service<F: ReceivedResources>` с методом на каждую операцию. Метод получает ресурсы
///   своего кадра `frame: &F`, например значок отправителя, и прочитанные параметры.
///   Реализация этого трейта - серверная сторона контракта;
/// - `fn dispatch(srv, incoming)` - разбор принятого кадра и вызов метода `Service` по ординалу
///   кадра;
/// - пустой `struct Protocol` с реализацией [`Protocol`](crate::Protocol);
/// - модуль на каждую операцию с `const CANONICAL` вида `"<протокол>.<операция>"` и
///   `const ORDINAL` - хэшем канонического имени из [`ordinal_of`](crate::ordinal_of). Модуль
///   операции `call(defer)` содержит ещё псевдоним `Reply<C>` её долга
///   [`Reply`](crate::Reply).
///
/// Doc-комментарий операции раскрытие переносит на метод клиента и метод `Service`.
///
/// Объявление протокола `Clock`. Имя `"Clock"` входит в каноническое имя каждой операции, а
/// модуль `clock` собирает порождённые элементы. Под объявлением - чтение ответа `now` у
/// клиента поверх канала вызывающего:
///
/// ```ignore
/// ipc::protocol! {
///     pub mod clock {
///         name = "Clock";
///
///         /// Текущее показание часов в наносекундах.
///         call fn now() -> u64;
///     }
/// }
///
/// fn read(channel: impl ipc::Channel) -> Result<u64, ipc::wire::IpcError> {
///     clock::Client::new(channel).now()
/// }
/// ```
///
/// Раскрытие проверяет const-ассертом на месте объявления, что кадр каждой операции
/// укладывается в [`wire::BODY_MAX`](crate::wire::BODY_MAX), а её параметры и их вложенные
/// записи - в пространство `field_id`. Длину строки `&str` и байтов `&[u8]` ограничивает только
/// кадр, и её проверяет запись кадра. Кадр, который не помещается, клиент не отправляет и
/// получает [`IpcError::FrameOverflow`](crate::wire::IpcError::FrameOverflow).
///
/// Клиент ждёт получателя до [`DEFAULT_OPERATION_TIMEOUT`], пока `Client::with_wait` не задаст
/// срок. Срок ограничивает приём одностороннего кадра и ответ на вызов. По истечении срока
/// операция возвращает [`IpcError::Timeout`](crate::wire::IpcError::Timeout).
///
/// Разбор кадра переносит capability каждого поля в прочитанное значение. Слот кадра берётся один
/// раз, и второе поле с тем же индексом отвергается как `InvalidValue`. Capability, которых не
/// назвало ни одно поле, закрываются вместе с кадром.
///
/// Сервис, который не читает ресурсов кадра, реализует трейт для ресурсов любого канала. Операция
/// `now` отвечает возвращённым значением, а `open` - долгом:
///
/// ```no_run
/// ipc::protocol! {
///     mod store {
///         name = "Store";
///
///         call fn now() -> u64;
///         call(defer) fn open() -> u32;
///     }
/// }
///
/// struct Server;
///
/// impl<F: ipc::ReceivedResources> store::Service<F> for Server {
///     fn now(&mut self, _frame: &F) -> u64 {
///         0
///     }
///
///     fn open(&mut self, _frame: &F, reply: store::open::Reply<F::Reply>) {
///         let _ = reply.send(7u32);
///     }
/// }
///
/// fn main() {}
/// ```
#[macro_export]
macro_rules! protocol {
    (
        $(#[$attr:meta])*
        $vis:vis mod $name:ident {
            name = $proto:literal ;
            $($ops:tt)*
        }
    ) => {
        $crate::protocol!(@ops [[$(#[$attr])*] $vis $name $proto] [] $($ops)*);
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
        $(#[$doc:meta])* call fn $op:ident ( $($params:tt)* ) $($rest:tt)*
    ) => {
        $crate::protocol!(@call
            ctx = $ctx done = $done docs = [$(#[$doc])*] op = $op params = [$($params)*]
            defer = [] rest = [$($rest)*]
        );
    };

    (@ops $ctx:tt $done:tt
        $(#[$doc:meta])* call ( defer ) fn $op:ident ( $($params:tt)* ) $($rest:tt)*
    ) => {
        $crate::protocol!(@call
            ctx = $ctx done = $done docs = [$(#[$doc])*] op = $op params = [$($params)*]
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

    (@call ctx = $ctx:tt done = [$($done:tt)*] docs = $docs:tt op = $op:ident
        params = [$($params:tt)*] defer = $defer:tt rest = [-> own Result < $ok:ty , $err:ty > ; $($rest:tt)*]
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [call $op $docs [::core::result::Result<$ok, $err>] [own] $defer [$ok , $err]]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@call ctx = $ctx:tt done = [$($done:tt)*] docs = $docs:tt op = $op:ident
        params = [$($params:tt)*] defer = $defer:tt rest = [-> Result < $ok:ty , $err:ty > ; $($rest:tt)*]
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [call $op $docs [::core::result::Result<$ok, $err>] [] $defer [$ok , $err]]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@call ctx = $ctx:tt done = [$($done:tt)*] docs = $docs:tt op = $op:ident
        params = [$($params:tt)*] defer = $defer:tt rest = [-> own $ret:ty ; $($rest:tt)*]
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [call $op $docs [$ret] [own] $defer []]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@call ctx = $ctx:tt done = [$($done:tt)*] docs = $docs:tt op = $op:ident
        params = [$($params:tt)*] defer = $defer:tt rest = [-> $ret:ty ; $($rest:tt)*]
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [call $op $docs [$ret] [] $defer []]
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
        [[$(#[$attr:meta])*] $vis:vis $name:ident $proto:literal]
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

                    $crate::__protocol_op_arms! { $kind $op $ret $ret_own $defer $params }
                }
            )*

            $crate::__protocol_client! {
                [
                    /// Клиент контракта поверх канала.
                ]
                Client Channel { [] __protocol_send __protocol_call [] }
                [$( { $kind $op $docs $ret $flat $params $owned } )*]
            }

            $crate::__protocol_client! {
                [
                    /// Клиент контракта поверх асинхронного канала. Метод операции возвращает
                    /// future с результатом метода [`Client`].
                ]
                AsyncClient AsyncChannel { [async] __protocol_send_async __protocol_call_async [. await] }
                [$( { $kind $op $docs $ret $flat $params $owned } )*]
            }

            /// Серверная сторона контракта над ресурсами кадра `F`.
            ///
            /// Метод каждой операции получает ресурсы своего кадра и прочитанные параметры.
            /// Capability кадра, которых не назвал ни один параметр, закрываются вместе с
            /// кадром после возврата метода.
            pub trait Service<F: $crate::ReceivedResources> {
                $(
                    $crate::__protocol_service_method! {
                        $kind $op $docs $ret $ret_own $defer $params
                    }
                )*
            }

            /// Разбирает принятый кадр и зовёт метод сервиса по ординалу кадра.
            ///
            /// `Err` называет отказ разбора кадра либо, у операции с возвратом значения, отказ
            /// доставки её ответа; итог доставки отложенного ответа видит только держатель долга.
            /// Кадр незнакомой операции отбрасывается вместе с каналом ответа, и вызыватель
            /// получает `PeerClosed`. Кадр операции с ответом без канала ответа отбрасывается без
            /// вызова метода.
            pub fn dispatch<F: $crate::ReceivedResources, S: Service<F>>(
                srv: &mut S,
                incoming: $crate::Incoming<'_, F>,
            ) -> ::core::result::Result<(), $crate::wire::IpcError> {
                let $crate::Incoming { bytes, mut resources } = incoming;
                let $crate::wire::FrameParts { header, body } = $crate::wire::Header::split(bytes)?;
                $(
                    if header.ordinal == $op::ORDINAL {
                        return $op::serve(srv, &mut resources, body);
                    }
                )*
                // Кадр незнакомой операции разрушается вместе с каналом ответа, и вызыватель
                // получает `PeerClosed`.
                ::core::mem::drop(resources);
                ::core::result::Result::Ok(())
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

/// Разбор кадра операции внутри её модуля и вызов метода сервиса.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_op_arms {
    (cast $op:ident [$ret:ty] $ret_own:tt $defer:tt
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        #[doc(hidden)]
        pub fn serve<F: $crate::ReceivedResources, S: super::Service<F>>(
            srv: &mut S,
            frame: &mut F,
            body: &[u8],
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            $crate::__protocol_decode_params!(body, frame, [$( { $field_id , $param , $wire_ty } )*]);
            srv.$op(&*frame $(, $param)*);
            ::core::result::Result::Ok(())
        }
    };

    (call $op:ident [$ret:ty] $ret_own:tt []
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        #[doc(hidden)]
        pub fn serve<F: $crate::ReceivedResources, S: super::Service<F>>(
            srv: &mut S,
            frame: &mut F,
            body: &[u8],
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            let ::core::option::Option::Some(channel) = $crate::ReceivedResources::take_reply(frame)
            else {
                return ::core::result::Result::Ok(());
            };
            $crate::__protocol_decode_params!(body, frame, [$( { $field_id , $param , $wire_ty } )*]);
            let value = srv.$op(&*frame $(, $param)*);
            $crate::Reply::<$ret, F::Reply>::__new(channel, ORDINAL)
                .send(value)
                .map_err($crate::wire::IpcError::from)
        }
    };

    (call $op:ident [$ret:ty] $ret_own:tt [defer]
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        #[doc(hidden)]
        pub fn serve<F: $crate::ReceivedResources, S: super::Service<F>>(
            srv: &mut S,
            frame: &mut F,
            body: &[u8],
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            let ::core::option::Option::Some(channel) = $crate::ReceivedResources::take_reply(frame)
            else {
                return ::core::result::Result::Ok(());
            };
            $crate::__protocol_decode_params!(body, frame, [$( { $field_id , $param , $wire_ty } )*]);
            srv.$op(&*frame $(, $param)*, Reply::__new(channel, ORDINAL));
            ::core::result::Result::Ok(())
        }

        /// Долг ответа этой операции над каналом `C`.
        pub type Reply<C> = $crate::Reply<$ret, C>;
    };
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

/// Клиент контракта над каналом с границей `$channel`: конструкторы, срок ожидания и метод на
/// каждую операцию. Вид методов задаёт первый токен [`__protocol_client_method`].
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_client {
    ([$(#[$doc:meta])*] $name:ident $channel:ident $mode:tt [$( { $($method:tt)* } )*]) => {
        $(#[$doc])*
        pub struct $name<C: $crate::$channel> {
            channel: C,
            wait: $crate::Timeout,
        }

        impl<C: $crate::$channel> $name<C> {
            /// Создаёт клиента поверх указанного канала со сроком
            /// [`DEFAULT_OPERATION_TIMEOUT`]($crate::DEFAULT_OPERATION_TIMEOUT).
            pub fn new(channel: C) -> Self {
                Self {
                    channel,
                    wait: $crate::DEFAULT_OPERATION_TIMEOUT,
                }
            }

            /// Задаёт срок ожидания получателя для всех операций клиента.
            #[must_use]
            pub fn with_wait(mut self, wait: $crate::Timeout) -> Self {
                self.wait = wait;
                self
            }

            /// Отдаёт канал обратно.
            pub fn into_channel(self) -> C {
                self.channel
            }

            $(
                $crate::__protocol_client_method! { $mode $($method)* }
            )*
        }
    };
}

/// Метод клиента для операции. Первый токен задаёт вид клиента: синхронный
/// `{ [] __protocol_send __protocol_call [] }` либо асинхронный
/// `{ [async] __protocol_send_async __protocol_call_async [. await] }`.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_client_method {
    ({ $async:tt $send:ident $call:ident [$($await:tt)*] }
     cast $op:ident $docs:tt $ret:tt $flat:tt
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*] []) => {
        $crate::__protocol_client_fn! {
            $async $docs $op [] (&self $(, $param: $sig_ty)*)
            [::core::result::Result<(), $crate::wire::IpcError>]
            {
                $crate::$send(
                    &self.channel,
                    $op::ORDINAL,
                    self.wait,
                    $crate::__protocol_write_params!(
                        [$( { $field_id , $param , $sig_ty , $wire_ty , $own } )*]
                    ),
                    |_, _| ::core::result::Result::Ok(()),
                )
                $($await)*
                .map_err(|undelivered| undelivered.error)
            }
        }
    };

    // Отказ отдаёт единственный владеющий параметр сам, а несколько - кортежем в порядке
    // объявления.
    ($mode:tt cast $op:ident $docs:tt $ret:tt $flat:tt $params:tt
     [{ $owned_id:expr , $generic:ident , $owned:ident , $owned_ty:ty }]) => {
        $crate::__protocol_client_method! {
            @owned_cast $mode $op $docs $params [{ $owned_id , $generic , $owned , $owned_ty }]
            [$crate::Received<$owned_ty, C::Resources>] [$owned]
        }
    };

    ($mode:tt cast $op:ident $docs:tt $ret:tt $flat:tt $params:tt
     [$( { $owned_id:expr , $generic:ident , $owned:ident , $owned_ty:ty } )+]) => {
        $crate::__protocol_client_method! {
            @owned_cast $mode $op $docs $params [$( { $owned_id , $generic , $owned , $owned_ty } )+]
            [($($crate::Received<$owned_ty, C::Resources>,)+)] [($($owned,)+)]
        }
    };

    (@owned_cast { $async:tt $send:ident $call:ident [$($await:tt)*] } $op:ident $docs:tt
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]
     [$( { $owned_id:expr , $generic:ident , $owned:ident , $owned_ty:ty } )+]
     [$refused:ty] [$refused_value:expr]) => {
        $crate::__protocol_client_fn! {
            $async $docs $op
            [$($generic: $crate::IntoWireValue<C::Resources, Read = $owned_ty>),+]
            (&self $(, $param: $sig_ty)*)
            [::core::result::Result<(), $crate::Undelivered<$refused>>]
            {
                $crate::$send(
                    &self.channel,
                    $op::ORDINAL,
                    self.wait,
                    $crate::__protocol_write_params!(
                        [$( { $field_id , $param , $sig_ty , $wire_ty , $own } )*]
                    ),
                    |body, frame| {
                        $crate::__protocol_decode_params!(
                            body, frame, [$( { $owned_id , $owned , $owned_ty } )+]
                        );
                        ::core::result::Result::Ok($refused_value)
                    },
                )
                $($await)*
            }
        }
    };

    ({ $async:tt $send:ident $call:ident [$($await:tt)*] }
     call $op:ident $docs:tt [$read:ty] [$ok:ty , $err:ty]
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]
     [$( { $owned_id:expr , $generic:ident , $owned:ident , $owned_ty:ty } )*]) => {
        $crate::__protocol_client_fn! {
            $async $docs $op
            [$($generic: $crate::IntoWireValue<C::Resources, Read = $owned_ty>),*]
            (&self $(, $param: $sig_ty)*)
            [::core::result::Result<
                $crate::Received<$ok, C::Resources>,
                $crate::CallError<$crate::Received<$err, C::Resources>>,
            >]
            {
                let mut frame = $crate::wire::MessageBuf::new();
                let answered = $crate::$call(
                    &self.channel,
                    $op::ORDINAL,
                    self.wait,
                    &mut frame,
                    $crate::__protocol_write_params!(
                        [$( { $field_id , $param , $sig_ty , $wire_ty , $own } )*]
                    ),
                )
                $($await)*;
                $crate::__protocol_outcome::<$ok, $err, _>($op::ORDINAL, answered?)
            }
        }
    };

    ({ $async:tt $send:ident $call:ident [$($await:tt)*] }
     call $op:ident $docs:tt [$read:ty] []
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]
     [$( { $owned_id:expr , $generic:ident , $owned:ident , $owned_ty:ty } )*]) => {
        $crate::__protocol_client_fn! {
            $async $docs $op
            [$($generic: $crate::IntoWireValue<C::Resources, Read = $owned_ty>),*]
            (&self $(, $param: $sig_ty)*)
            [::core::result::Result<$crate::Received<$read, C::Resources>, $crate::wire::IpcError>]
            {
                let mut frame = $crate::wire::MessageBuf::new();
                let answered = $crate::$call(
                    &self.channel,
                    $op::ORDINAL,
                    self.wait,
                    &mut frame,
                    $crate::__protocol_write_params!(
                        [$( { $field_id , $param , $sig_ty , $wire_ty , $own } )*]
                    ),
                )
                $($await)*;
                $crate::__protocol_answer::<$read, _>($op::ORDINAL, answered?)
            }
        }
    };
}

/// Сигнатура метода клиента. Метод асинхронного клиента возвращает `impl Future` с телом в
/// `async move` и принимает параметры с невыписанным временем жизни, которые `async fn`
/// отвергает.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_client_fn {
    ([] [$(#[$doc:meta])*] $op:ident [$($generics:tt)*] ($($args:tt)*) [$ret:ty] $body:block) => {
        $(#[$doc])*
        pub fn $op<$($generics)*>($($args)*) -> $ret $body
    };

    ([async] [$(#[$doc:meta])*] $op:ident [$($generics:tt)*] ($($args:tt)*) [$ret:ty] $body:block) => {
        $(#[$doc])*
        pub fn $op<$($generics)*>($($args)*) -> impl ::core::future::Future<Output = $ret> {
            async move $body
        }
    };
}

/// Метод серверного трейта для операции.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_service_method {
    (cast $op:ident [$(#[$doc:meta])*] $ret:tt $ret_own:tt $defer:tt
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        $(#[$doc])*
        fn $op(&mut self, frame: &F $(, $param: $crate::__protocol_received!($own F $wire_ty))*);
    };

    (call $op:ident [$(#[$doc:meta])*] [$ret:ty] [] []
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        $(#[$doc])*
        fn $op(&mut self, frame: &F $(, $param: $crate::__protocol_received!($own F $wire_ty))*)
            -> $ret;
    };

    (call $op:ident [$(#[$doc:meta])*] [$ret:ty] [own] []
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        $(#[$doc])*
        fn $op(&mut self, frame: &F $(, $param: $crate::__protocol_received!($own F $wire_ty))*)
            -> impl $crate::IntoWireValue<F, Read = $ret>;
    };

    (call $op:ident [$(#[$doc:meta])*] $ret:tt $ret_own:tt [defer]
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        $(#[$doc])*
        fn $op(
            &mut self,
            frame: &F
            $(, $param: $crate::__protocol_received!($own F $wire_ty))*,
            reply: $op::Reply<<F as $crate::ReceivedResources>::Reply>,
        );
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

fn one_way_frame<R: crate::ReceivedResources, V>(
    ordinal: u64,
    write: impl FnOnce(&mut crate::wire::MessageBuf<R>) -> Result<(), crate::wire::IpcError>,
) -> Result<crate::wire::MessageBuf<R>, crate::Undelivered<V>> {
    let mut frame = crate::wire::MessageBuf::new();
    build_frame(&mut frame, crate::wire::Header::new(ordinal), write)
        .map_err(crate::Undelivered::unwritten)?;
    Ok(frame)
}

/// Отправляет каналом односторонний кадр операции `ordinal` с записями параметров `write`.
/// Отказ отправки разбирает кадр чтением `read` в значение [`Undelivered`](crate::Undelivered),
/// отказ записи кадра закрывает capability параметров.
#[doc(hidden)]
pub fn __protocol_send<C: crate::Channel, V>(
    channel: &C,
    ordinal: u64,
    wait: crate::Timeout,
    write: impl FnOnce(&mut crate::wire::MessageBuf<C::Resources>) -> Result<(), crate::wire::IpcError>,
    read: impl FnOnce(&[u8], &mut C::Resources) -> Result<V, crate::wire::IpcError>,
) -> Result<(), crate::Undelivered<V>> {
    let mut frame = one_way_frame(ordinal, write)?;
    let sent = channel.send(frame.outgoing(), wait);
    sent.map_err(|unsent| crate::Undelivered::__new(unsent, read))
}

/// Отправляет каналом кадр вызова операции `ordinal`, построенный в `frame` записями параметров
/// `write`, и принимает кадр ответа в байты `frame`. Отказ разрушает кадр вызова вместе с его
/// capability.
#[doc(hidden)]
pub fn __protocol_call<'b, C: crate::Channel>(
    channel: &C,
    ordinal: u64,
    wait: crate::Timeout,
    frame: &'b mut crate::wire::MessageBuf<C::Resources>,
    write: impl FnOnce(&mut crate::wire::MessageBuf<C::Resources>) -> Result<(), crate::wire::IpcError>,
) -> Result<crate::Incoming<'b, C::Resources>, crate::wire::IpcError> {
    build_frame(frame, crate::wire::Header::new(ordinal), write)?;
    channel.call(frame.outgoing(), wait)
}

/// Отправляет односторонний кадр асинхронным каналом, как [`__protocol_send`].
#[doc(hidden)]
pub async fn __protocol_send_async<C: crate::AsyncChannel, V>(
    channel: &C,
    ordinal: u64,
    wait: crate::Timeout,
    write: impl FnOnce(&mut crate::wire::MessageBuf<C::Resources>) -> Result<(), crate::wire::IpcError>,
    read: impl FnOnce(&[u8], &mut C::Resources) -> Result<V, crate::wire::IpcError>,
) -> Result<(), crate::Undelivered<V>> {
    let mut frame = one_way_frame(ordinal, write)?;
    let sent = channel.send(frame.outgoing(), wait).await;
    sent.map_err(|unsent| crate::Undelivered::__new(unsent, read))
}

/// Отправляет кадр вызова асинхронным каналом, как [`__protocol_call`].
#[doc(hidden)]
pub async fn __protocol_call_async<'b, C: crate::AsyncChannel>(
    channel: &'b C,
    ordinal: u64,
    wait: crate::Timeout,
    frame: &'b mut crate::wire::MessageBuf<C::Resources>,
    write: impl FnOnce(&mut crate::wire::MessageBuf<C::Resources>) -> Result<(), crate::wire::IpcError>,
) -> Result<crate::Incoming<'b, C::Resources>, crate::wire::IpcError> {
    build_frame(frame, crate::wire::Header::new(ordinal), write)?;
    channel.call(frame.outgoing(), wait).await
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
