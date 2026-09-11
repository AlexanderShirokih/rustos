//! Объявление IPC-контракта.
//!
//! Сам макрос в типы полей не заглядывает - он только раскладывает объявление по местам. Что
//! с каким типом делать, решают трейты. [`IntoWireValue`](crate::IntoWireValue) пишет запись
//! поля, [`WireValue`](crate::WireValue) читает её, а [`WireTyped`](crate::WireTyped) задаёт
//! дескриптор типа. По дескрипторам полей const-функции [`crate::schema`] проверяют бюджет кадра
//! и пространство `field_id` каждой операции.

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
/// capability приходит из типа записи. Судьбу владеющего значения решает итог отправки. Успех
/// отдаёт capability получателю, отказ оставляет их отправителю, и освобождение значения их
/// закрывает. Клиентский метод `cast` с владеющими параметрами возвращает их при отказе в
/// [`Undelivered`](crate::Undelivered): один параметр - сам, несколько - кортежем в порядке
/// объявления. Отказ `call` владеющие параметры разрушает. `Timeout` вызова приходит и после того,
/// как сервер принял кадр, и тогда разрушаются параметры, чьи capability уже у сервера.
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
///   прочитанный ответ, метод `cast` - пустой успех. Ассоциированная функция
///   `Client::new(channel)` строит клиента, `with_wait` задаёт срок ожидания получателя,
///   `into_channel` отдаёт канал обратно;
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
/// записи - в пространство `field_id`.
///
/// Клиент ждёт получателя бессрочно, пока `Client::with_wait` не задаст срок. Срок ограничивает
/// приём одностороннего кадра и ответ на вызов, а `Timeout::POLL` означает poll. По истечении срока
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
/// use syscall::{Handle, kind};
///
/// ipc::protocol! {
///     mod store {
///         name = "Store";
///
///         call fn now() -> u64;
///         call(defer) fn open() -> own ipc::wire::Cap<kind::Port>;
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
///         let port = ipc::wire::Cap::from_handle(Handle::from_abi(core::num::NonZeroU32::MIN));
///         let _ = reply.send(port);
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
            head = [cast $op [$(#[$doc])*] [()] [] []]
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
        params = [$($params:tt)*] defer = $defer:tt rest = [-> own $ret:ty ; $($rest:tt)*]
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [call $op $docs [$ret] [own] $defer]
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
            head = [call $op $docs [$ret] [] $defer]
            acc = [] owned = [] names = [__Owned1 __Owned2 __Owned3 __Owned4]
            id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    // Тип клиентской сигнатуры и тип записи закрепляются за параметром. Владеющий параметр
    // получает обобщённый тип клиента из `names` и добавляется в список `owned`. По этому
    // списку клиент отпускает либо возвращает значения после отправки.

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
            owned = [$($owned)* { $generic , $param , $param_ty }]
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
        [$( { $kind:ident $op:ident $docs:tt $ret:tt $ret_own:tt $defer:tt $params:tt $owned:tt } )*]
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

            /// Клиент контракта поверх канала.
            pub struct Client<C: $crate::Channel> {
                channel: C,
                wait: $crate::Timeout,
            }

            impl<C: $crate::Channel> Client<C> {
                /// Создаёт клиента поверх указанного канала. Клиент ждёт получателя бессрочно.
                pub fn new(channel: C) -> Self {
                    Self {
                        channel,
                        wait: $crate::Timeout::INFINITE,
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
                    $crate::__protocol_client_method! { $kind $op $docs $ret $params $owned }
                )*
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
                    $param = ::core::option::Option::Some(
                        <$wire_ty as $crate::WireValue<'_, _>>::from_field(field.data, &mut *$frame)?,
                    );
                }
            )*
        }
        $(
            let $param = match $param {
                ::core::option::Option::Some(value) => value,
                ::core::option::Option::None => {
                    return ::core::result::Result::Err($crate::wire::IpcError::MissingField);
                }
            };
        )*
    };
}

/// Записи параметров операции в кадр `frame`.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_write_params {
    ([$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]) => {
        |frame: &mut $crate::wire::MessageBuf| {
            $( $crate::IntoWireValue::write_as_field(&$param, frame, $field_id)?; )*
            ::core::result::Result::Ok(())
        }
    };
}

/// Тип и значение владеющих параметров, которые односторонняя отправка возвращает при отказе.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_owned {
    (@ty [{ $generic:ident , $param:ident , $ty:ty }]) => {
        $generic
    };
    (@ty [$( { $generic:ident , $param:ident , $ty:ty } )+]) => {
        ($($generic,)+)
    };
    (@value [{ $generic:ident , $param:ident , $ty:ty }]) => {
        $param
    };
    (@value [$( { $generic:ident , $param:ident , $ty:ty } )+]) => {
        ($($param,)+)
    };
}

/// Переводит итог односторонней отправки в итог метода. Успех отпускает владеющие параметры,
/// отказ возвращает их в `Undelivered`.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_settle_owned {
    ($sent:expr, [$( { $generic:ident , $owned:ident , $owned_ty:ty } )+]) => {
        match $sent {
            ::core::result::Result::Ok(()) => {
                $( $crate::IntoWireValue::relinquish($owned); )+
                ::core::result::Result::Ok(())
            }
            ::core::result::Result::Err(error) => {
                ::core::result::Result::Err($crate::Undelivered {
                    error,
                    value: $crate::__protocol_owned!(
                        @value [$( { $generic , $owned , $owned_ty } )+]
                    ),
                })
            }
        }
    };
}

/// Метод клиента для операции.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_client_method {
    (cast $op:ident [$(#[$doc:meta])*] $ret:tt
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*] []) => {
        $(#[$doc])*
        pub fn $op(&self $(, $param: $sig_ty)*)
            -> ::core::result::Result<(), $crate::wire::IpcError>
        {
            $crate::__protocol_send(
                &self.channel,
                $op::ORDINAL,
                self.wait,
                $crate::__protocol_write_params!(
                    [$( { $field_id , $param , $sig_ty , $wire_ty , $own } )*]
                ),
            )
        }
    };

    (cast $op:ident [$(#[$doc:meta])*] $ret:tt
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]
     [$( { $generic:ident , $owned:ident , $owned_ty:ty } )+]) => {
        $(#[$doc])*
        pub fn $op<$($generic: $crate::IntoWireValue<Read = $owned_ty>),+>(
            &self $(, $param: $sig_ty)*
        ) -> ::core::result::Result<
            (),
            $crate::Undelivered<
                $crate::__protocol_owned!(@ty [$( { $generic , $owned , $owned_ty } )+])
            >,
        > {
            let sent = $crate::__protocol_send(
                &self.channel,
                $op::ORDINAL,
                self.wait,
                $crate::__protocol_write_params!(
                    [$( { $field_id , $param , $sig_ty , $wire_ty , $own } )*]
                ),
            );
            $crate::__protocol_settle_owned!(sent, [$( { $generic , $owned , $owned_ty } )+])
        }
    };

    (call $op:ident [$(#[$doc:meta])*] [$read:ty]
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty , $own:tt } )*]
     [$( { $generic:ident , $owned:ident , $owned_ty:ty } )*]) => {
        $(#[$doc])*
        pub fn $op<$($generic: $crate::IntoWireValue<Read = $owned_ty>),*>(
            &self $(, $param: $sig_ty)*
        ) -> ::core::result::Result<$crate::Received<$read, C::Resources>, $crate::wire::IpcError>
        {
            let mut reply = [0u8; $crate::wire::MESSAGE_INLINE_MAX];
            // `Ok` вызова означает, что сервер принял кадр вместе с его capability. `Timeout`
            // приходит и до приёма кадра сервером, и после него, поэтому при `Err` параметры
            // разрушаются в обоих случаях.
            let answered = $crate::__protocol_call(
                &self.channel,
                $op::ORDINAL,
                self.wait,
                &mut reply,
                $crate::__protocol_write_params!(
                    [$( { $field_id , $param , $sig_ty , $wire_ty , $own } )*]
                ),
            );
            if answered.is_ok() {
                $( $crate::IntoWireValue::relinquish($owned); )*
            }
            $crate::__protocol_answer::<$read, _>($op::ORDINAL, answered?)
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
            -> impl $crate::IntoWireValue<Read = $ret>;
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

pub(crate) fn send_frame<T>(
    header: crate::wire::Header,
    write: impl FnOnce(&mut crate::wire::MessageBuf) -> Result<(), crate::wire::IpcError>,
    send: impl FnOnce(&[u8], &[u32]) -> Result<T, crate::wire::IpcError>,
) -> Result<T, crate::wire::IpcError> {
    let mut frame = crate::wire::MessageBuf::new();
    frame.write_header(&header)?;
    write(&mut frame)?;
    frame.finish()?;
    send(frame.as_bytes(), frame.handles())
}

/// Отправляет каналом односторонний кадр операции `ordinal` с записями параметров `write`.
#[doc(hidden)]
pub fn __protocol_send<C: crate::Channel>(
    channel: &C,
    ordinal: u64,
    wait: crate::Timeout,
    write: impl FnOnce(&mut crate::wire::MessageBuf) -> Result<(), crate::wire::IpcError>,
) -> Result<(), crate::wire::IpcError> {
    send_frame(
        crate::wire::Header::new(ordinal),
        write,
        |bytes, handles| channel.send(bytes, handles, wait),
    )
}

/// Отправляет каналом кадр вызова операции `ordinal` и принимает в `reply` кадр ответа.
#[doc(hidden)]
pub fn __protocol_call<'b, C: crate::Channel>(
    channel: &C,
    ordinal: u64,
    wait: crate::Timeout,
    reply: &'b mut crate::FrameBytes,
    write: impl FnOnce(&mut crate::wire::MessageBuf) -> Result<(), crate::wire::IpcError>,
) -> Result<crate::Incoming<'b, C::Resources>, crate::wire::IpcError> {
    send_frame(
        crate::wire::Header::new(ordinal),
        write,
        |bytes, handles| channel.call(bytes, handles, reply, wait),
    )
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
    let crate::wire::FrameParts { header, body } = crate::wire::Header::split(bytes)?;
    if header.ordinal != ordinal {
        return Err(crate::wire::IpcError::InvalidValue);
    }

    let mut cursor = crate::wire::FieldCursor::new(body);
    let mut value = None;
    while let Some(field) = cursor.next_field()? {
        if field.id == crate::wire::FIELD_ID_MIN {
            value = Some(V::from_field(field.data, &mut resources)?);
        }
    }
    value.ok_or(crate::wire::IpcError::MissingField)
}
