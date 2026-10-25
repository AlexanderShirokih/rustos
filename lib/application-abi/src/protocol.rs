use crate::error::ApplicationError;

ipc::protocol! {
    /// Приложение - программа, которую открывают по имени. Программа экспортирует
    /// `Application` с фильтром `name`, и открытие экспорта запускает её, если она не запущена.
    /// По сессии `Application` диспетчер окон передаёт приложению окно.
    pub mod application {
        name = "Application";

        /// Передаёт приложению сессию `Display` окна `window` и сессию `Input` его ввода
        /// `input`. Ввод сессии приходит, пока окно видно. Приложение отвечает до первого
        /// вызова окна, потому что диспетчер окон обслуживает окно после ответа. Повторный вызов
        /// возвращает `Refused`.
        call fn show(
            own window: ipc::wire::Session<display_abi::display::Protocol>,
            own input: ipc::wire::Session<input_abi::input::Protocol>,
        ) -> Result<(), ApplicationError>;

        /// Завершается `PeerClosed` с уходом приложения. Второй ждущий вызов получает `Refused`.
        call async fn closed() -> Result<(), ApplicationError>;
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;
    use std::vec::Vec;

    use display_abi::display;
    use input_abi::input;
    use ipc::wire::Session;
    use ipc_test::{MockHandle, MockResources, complete};
    use syscall::kind;

    use super::{ApplicationError, application};

    type Window = Session<display::Protocol, MockHandle<kind::Port>>;

    type Input = Session<input::Protocol, MockHandle<kind::Port>>;

    const WINDOW: Window = Session::from_handle(MockHandle::new(0x57));
    const INPUT: Input = Session::from_handle(MockHandle::new(0x58));

    #[derive(Default)]
    struct Shown {
        sessions: RefCell<Vec<(u32, u32)>>,
    }

    #[allow(clippy::unused_async_trait_impl)]
    impl application::Service<MockResources> for Shown {
        fn show(
            &self,
            _frame: &MockResources,
            window: Window,
            input: Input,
        ) -> Result<(), ApplicationError> {
            self.sessions
                .borrow_mut()
                .push((window.into_handle().raw(), input.into_handle().raw()));
            Ok(())
        }

        async fn closed(&self, _frame: &MockResources) -> Result<(), ApplicationError> {
            Err(ApplicationError::Refused)
        }
    }

    #[test]
    fn show_carries_the_window_and_input_session_capabilities() {
        let (client_end, server_end) = ipc_test::pair();
        let shown = Shown::default();
        let shown = std::thread::scope(|scope| {
            let server = scope.spawn(move || {
                server_end
                    .serve(|frame| {
                        complete(application::dispatch(
                            &shown,
                            ipc::OwnedIncoming::new(frame).expect("frame fits"),
                        ))
                    })
                    .expect("dispatch ok");
                shown
            });

            let client = application::Client::new(client_end);
            assert_eq!(client.show(WINDOW, INPUT).wait(), Ok(()));
            server.join().expect("server joins")
        });
        assert_eq!(
            shown.sessions.into_inner(),
            [(WINDOW.into_handle().raw(), INPUT.into_handle().raw())]
        );
    }
}
