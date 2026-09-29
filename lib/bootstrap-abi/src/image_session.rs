//! Контракт [`image`] компонента с загрузочной программой и его отказ [`ImageError`].

use syscall::kind;

ipc::wire_enum! {
    /// Отказ в регионе образа.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ImageError {
        /// Копия capability не создана.
        NoSpace = 1,
        /// Incarnation, получившая сессию, завершилась.
        Refused = 2,
    }
}

ipc::protocol! {
    /// Сессия компонента к userland-образу.
    pub mod image {
        name = "Image";

        /// Выдаёт read-only capability на регион всего userland-образа. Регион действует до
        /// конца incarnation, получившей сессию, у всех держателей его копий. Повторный вызов
        /// выдаёт копию того же региона.
        call fn region() -> own Result<ipc::wire::Cap<kind::MemoryRegion>, ImageError>;
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use ipc::{IntoWireValue, wire::Cap};
    use ipc_test::{MockHandle, MockResources};
    use syscall::kind;

    use super::{ImageError, image};

    struct Sink;

    impl image::Service<MockResources> for Sink {
        fn region(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<Cap<kind::MemoryRegion>, ImageError>>
        {
            Ok::<_, ImageError>(MockHandle::<kind::MemoryRegion>::new(0x66))
        }
    }

    #[test]
    fn an_image_session_answers_with_the_region_of_the_image() {
        let (client_end, server_end) = ipc_test::pair();
        let client = image::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                server_end
                    .serve(|frame| image::dispatch(&mut Sink, frame))
                    .expect("dispatch ok");
            });
            assert_eq!(client.region(), Ok(Ok(MockHandle::new(0x66))));
        });
    }
}
