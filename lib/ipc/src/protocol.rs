//! Объявление IPC-контракта.
//!
//! Сам макрос в типы полей не заглядывает - он только раскладывает объявление по местам. Что
//! с каким типом делать, решают трейты. Кодирование поля берёт на себя
//! [`WireValue`](crate::WireValue) либо [`IntoWireHandle`](crate::IntoWireHandle). Форму
//! ответа задаёт [`WireReply`](crate::WireReply), а дескриптор типа -
//! [`WireTyped`](crate::WireTyped).
//! По этому дескриптору const-функции [`crate::schema`] проверяют бюджет кадра,
//! пространство `field_id` и совместимость с плоскостью.

/// Объявляет IPC-контракт: имя протокола, транспортную плоскость и операции.
///
/// Вид операции задаёт её ключевое слово. `call fn` - запрос с ответом, `cast fn` -
/// односторонняя операция от клиента к серверу, `event fn` - односторонняя операция от
/// сервера к клиенту.
///
/// Параметр, объявленный словом `Cap`, и возврат `-> Cap` передают владеющий хэндл через
/// [`IntoWireHandle`](crate::IntoWireHandle). Всякий другой параметр кодируется как
/// [`WireValue`](crate::WireValue), а форма ответа - как [`WireReply`](crate::WireReply).
///
/// Заголовок объявления задаёт ключи. `name = "Proto"` называет протокол и входит в
/// каноническое имя каждой операции, `plane = ring` переводит контракт на SPSC-кольцо,
/// `wait = N` задаёт дефолтный срок ожидания ответа. Контракт без ключа `plane` идёт
/// плоскостью `port`.
///
/// Раскрытие порождает модуль с именем и видимостью из объявления. Модуль встаёт на место
/// объявления и берёт окружающую область через `use super::*` - оттуда и приходят типы полей.
/// Внутри модуля лежат:
///
/// - `struct Client<T: Transport>` с методом на каждую `call` и `cast`. Метод `call`
///   возвращает [`Response`](crate::Response) с ответом, метод `cast` - пустой успех.
///   Ассоциированная функция `Client::new(transport)` строит клиента, `with_wait` меняет
///   срок ожидания ответа, `into_transport` отдаёт транспорт обратно;
/// - `trait Service` с методом на каждую `call` и `cast`. Реализация этого трейта -
///   серверная сторона контракта;
/// - `fn dispatch(srv, transport)` - приём одного кадра и вызов метода `Service` по ординалу
///   кадра;
/// - `trait Events` с методом на каждую `event` и `fn dispatch_event(handler, transport)`,
///   принимающий один кадр события;
/// - `struct EventSender` с ассоциированной функцией на каждую `event`. Первым аргументом
///   функция принимает транспорт;
/// - `const DESC` типа [`schema::ProtocolDesc`](crate::schema::ProtocolDesc) - операции,
///   их ординалы и раскладка полей. `const TRANSPORT_PLANE` содержит `"port"` либо `"ring"`;
/// - модуль на каждую операцию с `const CANONICAL` вида `"<протокол>.<операция>"` и
///   `const ORDINAL` - хэшем канонического имени из [`ordinal_of`](crate::ordinal_of).
///
/// Doc-комментарий операции раскрытие переносит на метод клиента, метод `Service` либо
/// `Events` и функцию `EventSender`.
///
/// Объявление протокола `Clock`. Имя `"Clock"` входит в каноническое имя каждой операции, а
/// модуль `clock` собирает порождённые элементы. Под объявлением - чтение ответа `now` у
/// клиента над транспортом вызывающего:
///
/// ```ignore
/// ipc::protocol! {
///     pub mod clock {
///         name = "Clock";
///
///         /// Текущее показание часов в наносекундах.
///         call fn now() -> u64;
///         /// Тик часов с его порядковым номером.
///         event fn tick(seq: u64);
///     }
/// }
///
/// fn read(transport: impl ipc::Transport) -> Result<u64, ipc::wire::IpcError> {
///     let client = clock::Client::new(transport);
///     Ok(client.now()?.value)
/// }
/// ```
///
/// Раскрытие проверяет три условия const-ассертом на месте объявления. Кадр самой объёмной
/// операции укладывается в [`wire::BODY_MAX`](crate::wire::BODY_MAX), число параметров
/// операции укладывается в пространство `field_id`, контракт плоскости `ring` обходится
/// операциями `cast` и `event`.
///
/// Срок ожидания ответа задаётся числом наносекунд. Ключ `wait = N` рядом с `name` задаёт
/// дефолт клиента, `call(wait = N)` перекрывает дефолт на отдельной операции, а
/// `Client::with_wait` меняет дефолт в рантайме. Значение `0` означает poll, объявление без
/// ключа `wait` ждёт ответ бессрочно. По истечении срока `call` возвращает
/// [`IpcError::Timeout`](crate::wire::IpcError::Timeout).
///
/// Судьбу владеющего хэндла решает итог отправки. Успешная отправка переносит хэндл в
/// таблицу получателя, и раскрытие зовёт [`relinquish`](crate::IntoWireHandle::relinquish).
/// После отказа хэндл остаётся у отправителя, и его закрывает `Drop` значения.
///
/// Капы принятого кадра принадлежат получателю. Каждый слот кадра потребляется разбором один
/// раз, и второе поле кадра с тем же индексом отвергается как `InvalidValue`. Капы, которых
/// разбор не потребил, закрываются вместе с кадром, и отдельной очистки обработчик не делает.
///
/// Объявление операции, отдающей владеющий хэндл. Слово `Cap` в возврате выбирает
/// [`IntoWireHandle`](crate::IntoWireHandle) на серверной стороне, а клиент забирает хэндл у
/// [`Response`](crate::Response) вызовом `claim`:
///
/// ```ignore
/// ipc::protocol! {
///     pub mod vault {
///         name = "Vault";
///
///         /// Открывает сессию хранилища и отдаёт её порт.
///         call fn open() -> Cap;
///     }
/// }
///
/// // Сервис реализует `fn open(&mut self) -> impl IntoWireHandle`.
/// // Клиент получает `Response<Cap, _>` и забирает хэндл вызовом `claim`.
/// ```
#[macro_export]
macro_rules! protocol {
    (
        $(#[$attr:meta])*
        $vis:vis mod $name:ident {
            name = $proto:literal $(, wait = $wait:literal)? $(, plane = $plane:ident)? ;
            $($ops:tt)*
        }
    ) => {
        $crate::protocol!(@ops
            [[$(#[$attr])*] $vis $name $proto
             [$crate::Timeout::INFINITE $(, $crate::Timeout::from_ns($wait))?]
             [port $(, $plane)?]]
            []
            $($ops)*
        );
    };

    // Каждое правило снимает одну операцию и передаёт её параметры в @params, который
    // возвращается сюда уже с разобранным списком. Формы возврата разведены отдельными
    // правилами: сопоставление идёт по правилам с откатом, чего внутри одного правила нет.

    (@ops $ctx:tt [$($done:tt)*]) => {
        $crate::protocol!(@emit $ctx [$($done)*]);
    };

    (@ops $ctx:tt [$($done:tt)*]
        $(#[$doc:meta])* cast fn $op:ident ( $($params:tt)* ) ; $($rest:tt)*
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [cast $op [$(#[$doc])*]
                    [()] [()] [$crate::__protocol_release] [$crate::__protocol_release] []]
            acc = [] id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@ops $ctx:tt [$($done:tt)*]
        $(#[$doc:meta])* event fn $op:ident ( $($params:tt)* ) ; $($rest:tt)*
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [event $op [$(#[$doc])*]
                    [()] [()] [$crate::__protocol_release] [$crate::__protocol_release] []]
            acc = [] id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@ops $ctx:tt [$($done:tt)*]
        $(#[$doc:meta])* call $(( wait = $wait:literal ))? fn $op:ident ( $($params:tt)* )
            -> Cap ; $($rest:tt)*
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [call $op [$(#[$doc])*]
                    [$crate::wire::Cap] [impl $crate::IntoWireHandle]
                    [$crate::WireReplyHandle] [$crate::IntoWireHandle::relinquish]
                    [$($wait)?]]
            acc = [] id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@ops $ctx:tt [$($done:tt)*]
        $(#[$doc:meta])* call $(( wait = $wait:literal ))? fn $op:ident ( $($params:tt)* )
            -> Result < $ok:ty , $err:ty > ; $($rest:tt)*
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [call $op [$(#[$doc])*]
                    [::core::result::Result<$ok, $err>] [::core::result::Result<$ok, $err>]
                    [$crate::WireReply] [$crate::__protocol_release]
                    [$($wait)?]]
            acc = [] id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    (@ops $ctx:tt [$($done:tt)*]
        $(#[$doc:meta])* call $(( wait = $wait:literal ))? fn $op:ident ( $($params:tt)* )
            -> $ret:ty ; $($rest:tt)*
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = [$($done)*] rest = [$($rest)*]
            head = [call $op [$(#[$doc])*]
                    [$ret] [$ret] [$crate::WireReply] [$crate::__protocol_release]
                    [$($wait)?]]
            acc = [] id = $crate::wire::FIELD_ID_MIN;
            $($params)*
        );
    };

    // За параметром закрепляются: тип клиентской сигнатуры, wire-тип, трейт кодирования и
    // способ отпустить значение после передачи. Дальше макрос типы не разбирает.

    (@params ctx = $ctx:tt done = [$($done:tt)*] rest = [$($rest:tt)*]
        head = [$($head:tt)*] acc = [$($acc:tt)*] id = $id:expr;
    ) => {
        $crate::protocol!(@ops $ctx [$($done)* { $($head)* [$($acc)*] }] $($rest)*);
    };

    (@params ctx = $ctx:tt done = $done:tt rest = $rest:tt
        head = $head:tt acc = [$($acc:tt)*] id = $id:expr;
        $param:ident : Cap $(, $($more:tt)*)?
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = $done rest = $rest head = $head
            acc = [$($acc)* { $id , $param , impl $crate::IntoWireHandle , $crate::wire::Cap ,
                              [$crate::IntoWireHandle] , [$crate::IntoWireHandle::relinquish] }]
            id = $id + 1u8;
            $($($more)*)?
        );
    };

    (@params ctx = $ctx:tt done = $done:tt rest = $rest:tt
        head = $head:tt acc = [$($acc:tt)*] id = $id:expr;
        $param:ident : $param_ty:ty $(, $($more:tt)*)?
    ) => {
        $crate::protocol!(@params
            ctx = $ctx done = $done rest = $rest head = $head
            acc = [$($acc)* { $id , $param , $param_ty , $param_ty , [$crate::WireValue] , [$crate::__protocol_release] }]
            id = $id + 1u8;
            $($($more)*)?
        );
    };

    (@emit
        [[$(#[$attr:meta])*] $vis:vis $name:ident $proto:literal
         [$wait:expr $(, $wait_override:expr)?] [$plane:ident $(, $plane_override:ident)?]]
        [$( {
            $kind:ident $op:ident [$(#[$doc:meta])*]
            [$read:ty] [$serve:ty] [$($reply_write:tt)*] [$($reply_release:tt)*] [$($op_wait:literal)?]
            [$( { $field_id:expr , $param:ident , $sig_ty:ty , $wire_ty:ty ,
                  [$($encode:tt)*] , [$($release_param:tt)*] } )*]
        } )*]
    ) => {
        $(#[$attr])*
        $vis mod $name {
            #[allow(unused_imports)]
            use super::*;

            $(
                /// Операция контракта: её ординал и обе стороны разбора кадра.
                pub mod $op {
                    #[allow(unused_imports)]
                    use super::super::*;

                    /// Каноническое имя операции: `<protocol>.<operation>`.
                    pub const CANONICAL: &::core::primitive::str =
                        concat!($proto, ".", stringify!($op));

                    /// Ординал операции: младшие 64 бита SHA-256 канонического имени.
                    pub const ORDINAL: u64 = $crate::ordinal_of(CANONICAL);

                    $crate::__protocol_op_arms! {
                        $kind $op [$read] [$($reply_write)*] [$($reply_release)*]
                        [$( { $field_id , $param , $wire_ty } )*]
                    }
                }
            )*

            /// Транспортная плоскость контракта: `"port"` либо `"ring"`.
            pub const TRANSPORT_PLANE: &::core::primitive::str = {
                let plane = stringify!($plane);
                $( let plane = stringify!($plane_override); )?
                plane
            };

            /// Дескриптор контракта: операции, их ординалы и раскладка полей.
            pub const DESC: $crate::schema::ProtocolDesc = $crate::schema::ProtocolDesc {
                operations: &[
                    $(
                        $crate::schema::OperationDesc {
                            name: stringify!($op),
                            canonical: $op::CANONICAL,
                            ordinal: $op::ORDINAL,
                            kind: $crate::__protocol_kind!($kind),
                            fields: &[
                                $(
                                    $crate::schema::FieldDesc {
                                        field_id: $field_id,
                                        field_type: <$wire_ty as $crate::WireTyped>::WIRE_TYPE,
                                    }
                                ),*
                            ],
                        }
                    ),*
                ],
            };

            const _: () = ::core::assert!(
                $crate::schema::fits_body_budget(&DESC, $crate::wire::BODY_MAX),
                "operation frame exceeds the MESSAGE_INLINE_MAX budget"
            );

            const _: () = ::core::assert!(
                $crate::schema::fits_field_id_space(&DESC, $crate::wire::FIELD_ID_MAX),
                "operation declares more parameters than the frame field_id space carries"
            );

            const _: () = ::core::assert!(
                !$crate::__protocol_is_ring!($plane $(, $plane_override)?)
                    || $crate::schema::is_one_way(&DESC),
                "ring transport carries no replies: declare only cast/event operations"
            );

            /// Клиент контракта поверх транспорта.
            pub struct Client<T: $crate::Transport> {
                transport: T,
                next_txid: ::core::cell::Cell<u32>,
                wait: $crate::Timeout,
            }

            impl<T: $crate::Transport> Client<T> {
                /// Создаёт клиента над транспортом. Срок ожидания ответа берётся из ключа
                /// `wait` объявления, счётчик `txid` начинается с единицы.
                pub fn new(transport: T) -> Self {
                    Self {
                        transport,
                        next_txid: ::core::cell::Cell::new(1u32),
                        wait: { let wait = $wait; $( let wait = $wait_override; )? wait },
                    }
                }

                /// Переопределяет дефолтный срок ожидания ответа. Операция с собственным
                /// `call(wait = N)` держит свой срок.
                #[must_use]
                pub fn with_wait(mut self, wait: $crate::Timeout) -> Self {
                    self.wait = wait;
                    self
                }

                /// Текущий дефолтный срок ожидания ответа.
                pub fn wait(&self) -> $crate::Timeout {
                    self.wait
                }

                /// Отдаёт транспорт обратно.
                pub fn into_transport(self) -> T {
                    self.transport
                }

                fn alloc_txid(&self) -> u32 {
                    let current = self.next_txid.get();
                    self.next_txid.set(current.checked_add(1).unwrap_or(1u32));
                    current
                }

                $(
                    $crate::__protocol_client_method! {
                        $kind $op [$(#[$doc])*] [$read] [$($op_wait)?]
                        [$( { $field_id , $param , $sig_ty , [$($encode)*] , [$($release_param)*] } )*]
                    }
                )*
            }

            /// Серверная сторона контракта.
            pub trait Service {
                $(
                    $crate::__protocol_service_method! {
                        $kind $op [$(#[$doc])*] [$serve] [$( { $param , $wire_ty } )*]
                    }
                )*
            }

            /// Принимает один кадр и отдаёт его сервису. Вызов не ждёт готовности
            /// транспорта. Пустой канал даёт `WouldBlock`, и ожидание остаётся за вызывающим.
            ///
            /// Неизвестный ординал закрывает канал кадром `FLAG_PEER_CLOSE`: контракт строгий.
            /// Принятый кадр с этим флагом даёт `PeerClosed`.
            pub fn dispatch<T: $crate::Transport, S: Service>(
                srv: &mut S,
                transport: &T,
            ) -> ::core::result::Result<(), $crate::wire::IpcError> {
                let mut bytes = [0u8; $crate::wire::MESSAGE_INLINE_MAX];
                let mut handles = [0u32; $crate::wire::MESSAGE_MAX_HANDLES];
                let len = transport.read_message(&mut bytes, &mut handles)?;
                let frame = &bytes[..len.bytes];
                let header = $crate::wire::Header::decode(frame)?;
                if header.has_flag($crate::wire::FLAG_PEER_CLOSE) {
                    return ::core::result::Result::Err($crate::wire::IpcError::PeerClosed);
                }
                let body = &frame[$crate::wire::HEADER_SIZE..];
                let slots = $crate::wire::HandleSlots::from_received(&handles[..len.handles]);
                $(
                    if header.ordinal == $op::ORDINAL {
                        return $op::serve(srv, transport, body, &slots, header.txid);
                    }
                )*
                $crate::__protocol_reject(transport, header.ordinal, header.txid)
            }

            /// Клиентская сторона событий контракта.
            pub trait Events {
                $(
                    $crate::__protocol_events_method! {
                        $kind $op [$(#[$doc])*] [$( { $param , $wire_ty } )*]
                    }
                )*
            }

            /// Принимает один кадр события и отдаёт его обработчику.
            ///
            /// Кадр не-события пропускается: событийная сторона канала строгости не требует.
            /// Такой кадр даёт успех, и канал остаётся открытым.
            pub fn dispatch_event<T: $crate::Transport, H: Events>(
                handler: &mut H,
                transport: &T,
            ) -> ::core::result::Result<(), $crate::wire::IpcError> {
                let mut bytes = [0u8; $crate::wire::MESSAGE_INLINE_MAX];
                let mut handles = [0u32; $crate::wire::MESSAGE_MAX_HANDLES];
                let len = transport.read_message(&mut bytes, &mut handles)?;
                let frame = &bytes[..len.bytes];
                let header = $crate::wire::Header::decode(frame)?;
                let body = &frame[$crate::wire::HEADER_SIZE..];
                let slots = $crate::wire::HandleSlots::from_received(&handles[..len.handles]);
                $(
                    if header.ordinal == $op::ORDINAL {
                        return $op::deliver(handler, body, &slots);
                    }
                )*
                ::core::result::Result::Ok(())
            }

            /// Отправитель событий контракта. Каждая функция принимает транспорт первым
            /// аргументом и пишет в него один кадр события.
            pub struct EventSender;

            impl EventSender {
                $(
                    $crate::__protocol_event_sender_method! {
                        $kind $op [$(#[$doc])*]
                        [$( { $field_id , $param , $sig_ty , [$($encode)*] , [$($release_param)*] } )*]
                    }
                )*
            }
        }
    };
}

/// Вид операции по её ключевому слову.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_kind {
    (call) => {
        $crate::schema::Kind::Call
    };
    (cast) => {
        $crate::schema::Kind::Cast
    };
    (event) => {
        $crate::schema::Kind::Event
    };
}

/// Задана ли транспортная плоскость кольцом.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_is_ring {
    ($plane:ident) => {
        $crate::__protocol_is_ring!(@is $plane)
    };
    ($plane:ident, $override:ident) => {
        $crate::__protocol_is_ring!(@is $override)
    };
    (@is port) => {
        false
    };
    (@is ring) => {
        true
    };
}

/// Методы операции внутри её модуля: серверная и событийная стороны одного ординала.
///
/// Обе стороны объявлены для каждой операции: `dispatch` зовёт `serve`, `dispatch_event` -
/// `deliver`, и вид операции решает, обслуживает вызов или отвергает кадр.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_op_arms {
    // Событие на серверной стороне - чужой ординал, и канал закрывается как на неизвестном.
    (event $op:ident [$read:ty] [$($reply_write:tt)*] [$($reply_release:tt)*]
     [$( { $field_id:expr , $param:ident , $wire_ty:ty } )*]) => {
        /// Отвергает кадр: операция односторонняя от сервера к клиенту.
        pub fn serve<T: $crate::Transport, S: super::Service>(
            _srv: &mut S,
            transport: &T,
            _body: &[u8],
            _slots: &$crate::wire::HandleSlots,
            txid: u32,
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            $crate::__protocol_reject(transport, ORDINAL, txid)
        }

        /// Разбирает кадр события и отдаёт его обработчику.
        pub fn deliver<H: super::Events>(
            handler: &mut H,
            body: &[u8],
            slots: &$crate::wire::HandleSlots,
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            $crate::__protocol_decode_params!(body, slots, [$( { $field_id , $param , $wire_ty } )*]);
            handler.$op($($param),*);
            ::core::result::Result::Ok(())
        }
    };

    (cast $op:ident [$read:ty] [$($reply_write:tt)*] [$($reply_release:tt)*]
     [$( { $field_id:expr , $param:ident , $wire_ty:ty } )*]) => {
        /// Разбирает кадр и отдаёт вызов сервису. Ответа односторонняя операция не шлёт.
        pub fn serve<T: $crate::Transport, S: super::Service>(
            srv: &mut S,
            _transport: &T,
            body: &[u8],
            slots: &$crate::wire::HandleSlots,
            _txid: u32,
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            $crate::__protocol_decode_params!(body, slots, [$( { $field_id , $param , $wire_ty } )*]);
            srv.$op($($param),*);
            ::core::result::Result::Ok(())
        }

        /// Отбрасывает кадр. У клиента нет этой операции.
        pub fn deliver<H: super::Events>(
            _handler: &mut H,
            _body: &[u8],
            _slots: &$crate::wire::HandleSlots,
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            ::core::result::Result::Ok(())
        }
    };

    (call $op:ident [$read:ty] [$($reply_write:tt)*] [$($reply_release:tt)*]
     [$( { $field_id:expr , $param:ident , $wire_ty:ty } )*]) => {
        /// Разбирает кадр, зовёт сервис и отправляет ответ.
        ///
        /// Хэндл ответа отпускается только при успешной передаче: `Err` значит, что ядро его
        /// не тронуло, и владение остаётся у сервиса.
        pub fn serve<T: $crate::Transport, S: super::Service>(
            srv: &mut S,
            transport: &T,
            body: &[u8],
            slots: &$crate::wire::HandleSlots,
            txid: u32,
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            $crate::__protocol_decode_params!(body, slots, [$( { $field_id , $param , $wire_ty } )*]);
            let value = srv.$op($($param),*);
            let mut out = $crate::wire::HandleSlots::new();
            let sent: ::core::result::Result<(), $crate::wire::IpcError> = (|| {
                let mut reply =
                    $crate::wire::MessageBuf::<{ $crate::wire::MESSAGE_INLINE_MAX }>::new();
                reply.write_header(&$crate::wire::Header::new(
                    ORDINAL,
                    txid,
                    $crate::wire::FLAG_RESPONSE | $($reply_write)*::reply_flags(&value),
                ))?;
                $($reply_write)*::write_reply(&value, &mut reply, &mut out)?;
                reply.finish()?;
                transport.write_message(reply.as_bytes(), out.as_slice())
            })();
            if sent.is_ok() {
                $($reply_release)*(value);
            }
            sent
        }

        /// Отбрасывает кадр. У клиента нет этой операции.
        pub fn deliver<H: super::Events>(
            _handler: &mut H,
            _body: &[u8],
            _slots: &$crate::wire::HandleSlots,
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            ::core::result::Result::Ok(())
        }
    };
}

/// Разбирает тело кадра в объявленные параметры операции.
///
/// Раскрывается в объявления с именами параметров: следом их видно как обычные значения.
/// Отсутствие объявленного поля - `MissingField`, неизвестное поле пропускается курсором.
/// Второе поле тела с тем же `field_id` курсор отвергает как `FieldOrder`.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_decode_params {
    ($body:expr, $slots:expr, [$( { $field_id:expr , $param:ident , $wire_ty:ty } )*]) => {
        $( let mut $param: ::core::option::Option<$wire_ty> = ::core::option::Option::None; )*
        let mut cursor = $crate::wire::FieldCursor::new($body);
        while let ::core::option::Option::Some(field) = cursor.next_field()? {
            $(
                if field.id == $field_id {
                    $param = ::core::option::Option::Some(
                        <$wire_ty as $crate::WireValue>::from_field(field.data, $slots)?,
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

/// Метод клиента для операции.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_client_method {
    // Событие приходит от сервера: у клиента метода нет.
    (event $($rest:tt)*) => {};

    (cast $op:ident [$(#[$doc:meta])*] [$read:ty] [$($wait:literal)?]
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , [$($encode:tt)*] , [$($release_param:tt)*] } )*]) => {
        $(#[$doc])*
        pub fn $op(&self $(, $param: $sig_ty)*)
            -> ::core::result::Result<(), $crate::wire::IpcError>
        {
            let mut out = $crate::wire::HandleSlots::new();
            let sent: ::core::result::Result<(), $crate::wire::IpcError> = (|| {
                let mut buf =
                    $crate::wire::MessageBuf::<{ $crate::wire::MESSAGE_INLINE_MAX }>::new();
                buf.write_header(&$crate::wire::Header::new($op::ORDINAL, 0u32, 0u16))?;
                $( $($encode)*::write_as_field(&$param, &mut buf, $field_id, &mut out)?; )*
                buf.finish()?;
                self.transport.write_message(buf.as_bytes(), out.as_slice())
            })();
            // Владение капами уходит только при успешной передаче: иначе их закроет `Drop`.
            if sent.is_ok() {
                $( $($release_param)*($param); )*
            }
            sent
        }
    };

    (call $op:ident [$(#[$doc:meta])*] [$read:ty] [$($wait:literal)?]
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , [$($encode:tt)*] , [$($release_param:tt)*] } )*]) => {
        $(#[$doc])*
        pub fn $op(&self $(, $param: $sig_ty)*)
            -> ::core::result::Result<$crate::Response<$read, T::Resources>, $crate::wire::IpcError>
        {
            let txid = self.alloc_txid();
            // Срок ожидания ответа: дефолт клиента, если операция не задала собственный.
            let wait = self.wait;
            $( let wait = $crate::Timeout::from_ns($wait); )?
            let mut out = $crate::wire::HandleSlots::new();
            // Перенос кап происходит в первом `wait_readable`: `Ok` значит, что ядро их
            // забрало, `Err` - что не тронуло.
            let request: ::core::result::Result<(), $crate::wire::IpcError> = (|| {
                let mut buf =
                    $crate::wire::MessageBuf::<{ $crate::wire::MESSAGE_INLINE_MAX }>::new();
                buf.write_header(&$crate::wire::Header::new($op::ORDINAL, txid, 0u16))?;
                $( $($encode)*::write_as_field(&$param, &mut buf, $field_id, &mut out)?; )*
                buf.finish()?;
                self.transport.write_message(buf.as_bytes(), out.as_slice())?;
                self.transport.wait_readable(wait)
            })();
            if request.is_ok() {
                $( $($release_param)*($param); )*
            }
            request?;

            let mut bytes = [0u8; $crate::wire::MESSAGE_INLINE_MAX];
            let mut handles = [0u32; $crate::wire::MESSAGE_MAX_HANDLES];
            loop {
                let read = self.transport.read_message(&mut bytes, &mut handles);
                let mut resources = self.transport.take_resources();
                let len = read?;
                let frame = &bytes[..len.bytes];
                let header = $crate::wire::Header::decode(frame)?;
                if header.has_flag($crate::wire::FLAG_PEER_CLOSE) {
                    return ::core::result::Result::Err($crate::wire::IpcError::PeerClosed);
                }
                // Ответ обязан нести RESPONSE и тот же txid, иначе кадр отброшен.
                if !header.has_flag($crate::wire::FLAG_RESPONSE) || header.txid != txid {
                    ::core::mem::drop(resources);
                    self.transport.wait_readable(wait)?;
                    continue;
                }
                let body = &frame[$crate::wire::HEADER_SIZE..];
                let slots = $crate::wire::HandleSlots::from_received(&handles[..len.handles]);
                let value = <$read as $crate::WireReply>::read_reply(&header, body, &slots)?;
                $crate::ReceivedResources::retain_decoded(&mut resources, &slots);
                return ::core::result::Result::Ok($crate::Response::new(value, resources));
            }
        }
    };
}

/// Метод серверного трейта для операции.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_service_method {
    (event $($rest:tt)*) => {};

    (cast $op:ident [$(#[$doc:meta])*] [$serve:ty] [$( { $param:ident , $wire_ty:ty } )*]) => {
        $(#[$doc])*
        fn $op(&mut self $(, $param: $wire_ty)*);
    };

    (call $op:ident [$(#[$doc:meta])*] [$serve:ty] [$( { $param:ident , $wire_ty:ty } )*]) => {
        $(#[$doc])*
        fn $op(&mut self $(, $param: $wire_ty)*) -> $serve;
    };
}

/// Метод клиентского трейта событий.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_events_method {
    (call $($rest:tt)*) => {};
    (cast $($rest:tt)*) => {};

    (event $op:ident [$(#[$doc:meta])*] [$( { $param:ident , $wire_ty:ty } )*]) => {
        $(#[$doc])*
        fn $op(&mut self $(, $param: $wire_ty)*);
    };
}

/// Метод серверного эмиттера событий.
#[doc(hidden)]
#[macro_export]
macro_rules! __protocol_event_sender_method {
    (call $($rest:tt)*) => {};
    (cast $($rest:tt)*) => {};

    (event $op:ident [$(#[$doc:meta])*]
     [$( { $field_id:expr , $param:ident , $sig_ty:ty , [$($encode:tt)*] , [$($release_param:tt)*] } )*]) => {
        $(#[$doc])*
        pub fn $op<T: $crate::Transport>(transport: &T $(, $param: $sig_ty)*)
            -> ::core::result::Result<(), $crate::wire::IpcError>
        {
            let mut out = $crate::wire::HandleSlots::new();
            let sent: ::core::result::Result<(), $crate::wire::IpcError> = (|| {
                let mut buf =
                    $crate::wire::MessageBuf::<{ $crate::wire::MESSAGE_INLINE_MAX }>::new();
                buf.write_header(&$crate::wire::Header::new($op::ORDINAL, 0u32, 0u16))?;
                $( $($encode)*::write_as_field(&$param, &mut buf, $field_id, &mut out)?; )*
                buf.finish()?;
                transport.write_message(buf.as_bytes(), out.as_slice())
            })();
            if sent.is_ok() {
                $( $($release_param)*($param); )*
            }
            sent
        }
    };
}

/// Отпускает значение, не передававшее владения.
///
/// Парная форма к [`IntoWireHandle::relinquish`](crate::IntoWireHandle::relinquish). У поля,
/// которое не capability, забирать нечего, и `protocol!` зовёт обе одинаково.
#[doc(hidden)]
pub fn __protocol_release<T>(_value: T) {}

/// Отвергает кадр с чужим ординалом, закрывая канал.
///
/// Контракт строгий. Незнакомая операция не пропускается молча, а гасит канал кадром
/// `FLAG_PEER_CLOSE` в ответ.
#[doc(hidden)]
pub fn __protocol_reject<T: crate::Transport>(
    transport: &T,
    ordinal: u64,
    txid: u32,
) -> Result<(), crate::wire::IpcError> {
    let mut reply = crate::wire::MessageBuf::<{ crate::wire::MESSAGE_INLINE_MAX }>::new();
    reply.write_header(&crate::wire::Header::new(
        ordinal,
        txid,
        crate::wire::FLAG_PEER_CLOSE,
    ))?;
    reply.finish()?;
    transport.write_message(reply.as_bytes(), &[])
}
