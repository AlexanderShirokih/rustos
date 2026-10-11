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
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use display_abi::display;
    use input_abi::input;
    use ipc::wire::Session;
    use ipc_test::{MockHandle, MockResources};
    use syscall::kind;

    use super::{ApplicationError, application};

    type Window = Session<display::Protocol, MockHandle<kind::Port>>;

    type Input = Session<input::Protocol, MockHandle<kind::Port>>;

    const WINDOW: Window = Session::from_handle(MockHandle::new(0x57));
    const INPUT: Input = Session::from_handle(MockHandle::new(0x58));

    #[derive(Default)]
    struct Shown {
        sessions: Vec<(u32, u32)>,
    }

    impl application::Service<MockResources> for Shown {
        fn show(
            &mut self,
            _frame: &MockResources,
            window: Window,
            input: Input,
        ) -> Result<(), ApplicationError> {
            self.sessions
                .push((window.into_handle().raw(), input.into_handle().raw()));
            Ok(())
        }
    }

    #[test]
    fn show_carries_the_window_and_input_session_capabilities() {
        let (client_end, server_end) = ipc_test::pair();
        let mut shown = Shown::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                server_end
                    .serve(|frame| application::dispatch(&mut shown, frame))
                    .expect("dispatch ok");
            });

            let client = application::Client::new(client_end);
            assert_eq!(client.show(WINDOW, INPUT), Ok(()));
            server.join().expect("server joins");
        });
        assert_eq!(
            shown.sessions,
            [(WINDOW.into_handle().raw(), INPUT.into_handle().raw())]
        );
    }
}
