//! Протокол [`component_startup`]. Компонент запрашивает им у родителя ресурсы по имени
//! требования, а провайдер принимает им заявки импортёров на свой экспорт.
//!
//! Код выхода 0 сообщает родителю, что компонент закончил работу, ненулевой код - об отказе.

use ipc::{
    WireTyped,
    schema::max_body_len,
    wire::{BODY_MAX, Str},
};
use syscall::kind;

/// Граница длины имени требования в байтах.
pub const REQUIREMENT_NAME_MAX: usize = 64;

/// Срок одного запроса компонента к родителю по стартовой сессии.
pub const STARTUP_REQUEST_TIMEOUT: ipc::Timeout = ipc::Timeout::from_secs(5);

/// Граница длины идентификатора протокола [`ipc::Protocol::ID`] в байтах.
pub const PROTOCOL_ID_MAX: usize = 64;

/// Имя, под которым компонент запрашивает сессию журнала с протоколом `Log`. Манифест это имя
/// не объявляет. Родитель без журнала отвечает на запрос `Refused`.
pub const LOG_REQUIREMENT: &str = "log";

ipc::wire_enum! {
    /// Отказ в сессии требования. `Unbound`, `Refused` и `Unavailable` окончательны.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SessionError {
        /// Повтор запроса заменил заявку либо не хватило ресурса.
        NoSpace = 1,
        /// Имя, вид или протокол недопустимы, выбор провайдера неоднозначен.
        Refused = 2,
        /// Провайдер требования снят навсегда.
        Unavailable = 3,
        /// Индекс равен числу провайдеров требования или превышает его.
        Unbound = 4,
    }
}

ipc::wire_enum! {
    /// Отказ в заявке на экспорт.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AcceptError {
        /// Повтор `accept` тем же провайдером заменил этот запрос.
        NoSpace = 1,
        /// Экспорт вне манифеста вызывающего либо вызов не по стартовой сессии.
        Refused = 2,
    }
}

ipc::wire_value! {
    /// Имя требования, протокол сессии и номер провайдера в списке требования.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SessionRequirement<'a> {
        pub name: Str<'a, REQUIREMENT_NAME_MAX>,
        pub protocol: Str<'a, PROTOCOL_ID_MAX>,
        pub index: u16,
    }
}

ipc::wire_value! {
    /// Заявка импортёра на экспорт провайдера.
    #[derive(Debug)]
    pub struct SessionRequest<R = ipc::Reply<Result<ipc::wire::Cap<kind::Port>, SessionError>>> {
        /// Ответ на операцию `session` импортёра.
        pub reply: R,
    }
}

const _: () = assert!(
    max_body_len(&[<SessionRequirement<'static> as WireTyped>::WIRE_TYPE]) <= BODY_MAX,
    "session request frame exceeds the body budget"
);

const _: () = assert!(
    max_body_len(&[<Result<SessionRequest, AcceptError> as WireTyped>::WIRE_TYPE]) <= BODY_MAX,
    "accept response frame exceeds the body budget"
);

ipc::protocol! {
    /// Стартовая сессия ребёнка к родителю.
    pub mod component_startup {
        name = "ComponentStartup";

        /// Выдаёт сессию провайдера `requirement.index`. Индекс, равный числу провайдеров
        /// требования или превышающий его, даёт `Unbound`. Повтор имени и индекса заменяет
        /// ожидающую заявку импортёра и отвечает на прежнюю `NoSpace`.
        call(defer) fn session(
            requirement: SessionRequirement,
        ) -> own Result<ipc::wire::Cap<kind::Port>, SessionError>;

        /// Выдаёт заявку импортёра на экспорт `export`. Импортёр получает ответ на заявку как
        /// ответ своей операции `session`.
        call(defer) fn accept(
            export: Str<{ PROTOCOL_ID_MAX }>,
        ) -> own Result<SessionRequest, AcceptError>;
    }
}

#[cfg(test)]
mod tests {
    use std::{string::String, vec::Vec};

    use ipc::wire::{Cap, Str};
    use ipc_test::{MockHandle, MockReply, MockResources};
    use syscall::kind;

    use super::{
        AcceptError, PROTOCOL_ID_MAX, SessionError, SessionRequest, SessionRequirement,
        component_startup,
    };

    const SESSION_CAP: MockHandle<kind::Port> = MockHandle::new(0x28);
    const EXPORT: &str = "FileSystem";
    const REQUIREMENT: &str = "files";

    #[derive(Debug, PartialEq, Eq)]
    struct RequestedSession {
        name: String,
        protocol: String,
        index: u16,
    }

    #[derive(Default)]
    struct Parent {
        requirements: Vec<RequestedSession>,
        exports: Vec<String>,
        waiting: Option<component_startup::session::Reply<MockReply>>,
    }

    impl component_startup::Service<MockResources> for Parent {
        fn session(
            &mut self,
            _frame: &MockResources,
            requirement: SessionRequirement<'_>,
            reply: component_startup::session::Reply<MockReply>,
        ) {
            self.requirements.push(RequestedSession {
                name: requirement.name.as_str().into(),
                protocol: requirement.protocol.as_str().into(),
                index: requirement.index,
            });
            self.waiting = Some(reply);
        }

        fn accept(
            &mut self,
            _frame: &MockResources,
            export: Str<'_, PROTOCOL_ID_MAX>,
            reply: component_startup::accept::Reply<MockReply>,
        ) {
            self.exports.push(export.as_str().into());
            let Some(debt) = self.waiting.take() else {
                let _ = reply.send(Err::<
                    SessionRequest<component_startup::session::Reply<MockReply>>,
                    _,
                >(AcceptError::Refused));
                return;
            };
            reply
                .send(Ok::<_, AcceptError>(SessionRequest { reply: debt }))
                .expect("the parent answers accept");
        }
    }

    #[test]
    fn an_accept_response_carries_the_reply_of_the_request() {
        let (importer_end, importer_listener) = ipc_test::pair();
        let (provider_end, provider_listener) = ipc_test::pair();
        let mut parent = Parent::default();

        let answer = std::thread::scope(|scope| {
            let importer = scope.spawn(|| {
                let requirement = SessionRequirement {
                    name: Str::new(REQUIREMENT).expect("name within the bound"),
                    protocol: Str::new(EXPORT).expect("protocol within the bound"),
                    index: 7,
                };
                component_startup::Client::new(importer_end).session(requirement)
            });
            importer_listener
                .serve(|frame| component_startup::dispatch(&mut parent, frame))
                .expect("dispatch ok");

            let provider = scope.spawn(|| {
                component_startup::Client::new(provider_end)
                    .accept(Str::new(EXPORT).expect("export within the bound"))
            });
            provider_listener
                .serve(|frame| component_startup::dispatch(&mut parent, frame))
                .expect("dispatch ok");

            let request = provider
                .join()
                .expect("provider joins")
                .expect("accept call")
                .expect("a request arrives");
            let reply: ipc::Reply<Result<Cap<kind::Port>, SessionError>, MockReply> = request.reply;
            reply
                .send(Ok::<_, SessionError>(SESSION_CAP))
                .expect("the provider answers");
            importer.join().expect("importer joins")
        });

        assert_eq!(answer, Ok(Ok(SESSION_CAP)));
        assert_eq!(
            parent.requirements,
            [RequestedSession {
                name: REQUIREMENT.into(),
                protocol: EXPORT.into(),
                index: 7,
            }]
        );
        assert_eq!(parent.exports, [String::from(EXPORT)]);
    }
}
