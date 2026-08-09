//! Объявление IPC-контракта.
//!
//! Раскрытие типы не разбирает: кодирование поля выбирает трейт ([`WireValue`](crate::WireValue)
//! либо [`IntoWireHandle`](crate::IntoWireHandle)), форму ответа - [`WireReply`](crate::WireReply),
//! дескриптор типа - [`WireTyped`](crate::WireTyped), а бюджет кадра, пространство `field_id` и
//! совместимость с плоскостью проверяют const-функции [`crate::schema`] по дескриптору.

/// Разворачивает объявление контракта в модуль с `Client`, `Service`, `dispatch`, `Events`,
/// `EventSender`, `dispatch_event`, дескриптором `DESC`, `TRANSPORT_PLANE` и модулем на
/// операцию с её `ORDINAL` и `CANONICAL`.
///
/// Синтаксис объявления описан в
/// [docs/ipc.md](https://github.com/AlexanderShirokih/molecular/blob/main/docs/ipc.md).
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
                /// Заводит клиента поверх транспорта.
                pub fn new(transport: T) -> Self {
                    Self {
                        transport,
                        next_txid: ::core::cell::Cell::new(1u32),
                        wait: { let wait = $wait; $( let wait = $wait_override; )? wait },
                    }
                }

                /// Переопределяет дефолтный срок ожидания ответа. Операции с собственным
                /// `call(wait = N)` его не замечают.
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

            /// Принимает один кадр и отдаёт его сервису.
            ///
            /// Неизвестный ординал закрывает канал кадром `FLAG_PEER_CLOSE`: контракт строгий.
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

            /// Серверный эмиттер событий контракта.
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

        /// Отбрасывает кадр: у клиента этой операции нет.
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

        /// Отбрасывает кадр: у клиента этой операции нет.
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
            -> ::core::result::Result<$read, $crate::wire::IpcError>
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
                let len = self.transport.read_message(&mut bytes, &mut handles)?;
                let frame = &bytes[..len.bytes];
                let header = $crate::wire::Header::decode(frame)?;
                if header.has_flag($crate::wire::FLAG_PEER_CLOSE) {
                    return ::core::result::Result::Err($crate::wire::IpcError::PeerClosed);
                }
                // Ответ обязан нести RESPONSE и тот же txid, иначе кадр отброшен.
                if !header.has_flag($crate::wire::FLAG_RESPONSE) || header.txid != txid {
                    self.transport.wait_readable(wait)?;
                    continue;
                }
                let body = &frame[$crate::wire::HEADER_SIZE..];
                let slots = $crate::wire::HandleSlots::from_received(&handles[..len.handles]);
                return <$read as $crate::WireReply>::read_reply(&header, body, &slots);
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
/// Парная форма к [`IntoWireHandle::relinquish`](crate::IntoWireHandle::relinquish): у поля,
/// которое не capability, забирать нечего, и `protocol!` зовёт обе одинаково.
#[doc(hidden)]
pub fn __protocol_release<T>(_value: T) {}

/// Отвергает кадр с чужим ординалом, закрывая канал.
///
/// Контракт строгий: незнакомая операция не пропускается молча, а гасит канал кадром
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
