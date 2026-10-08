//! Контракт [`features`] записи плана с реестром признаков машины и его отказ
//! [`FeaturesError`].

/// Предельная длина признака в байтах. Предел равен наибольшей длине признака в манифесте.
/// Строка длиннее предела проходит транспорт и получает [`FeaturesError::Invalid`].
pub const FEATURE_MAX_BYTES: usize = u8::MAX as usize;

ipc::wire_enum! {
    /// Отказ в публикации признака.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FeaturesError {
        /// Строка пуста либо длиннее [`FEATURE_MAX_BYTES`].
        Invalid = 1,
        /// Реестр признаков заполнен.
        NoSpace = 2,
    }
}

ipc::protocol! {
    /// Сессия записи плана к реестру признаков машины.
    pub mod features {
        name = "Features";

        /// Публикует признак `feature`. Признак - непрозрачная строка, которую реестр
        /// сравнивает байтами. Повторная публикация успешна без второй записи. Признак
        /// снимается с концом incarnation, получившей сессию.
        call fn publish(feature: ipc::wire::Str<{ ipc::wire::FIELD_DATA_MAX }>) -> Result<(), FeaturesError>;
    }
}

#[cfg(test)]
mod tests {
    use std::{string::String, thread, vec::Vec};

    use ipc::wire::Str;
    use ipc_test::MockResources;

    use super::{FEATURE_MAX_BYTES, FeaturesError, features};

    #[derive(Default)]
    struct Registry {
        published: Vec<String>,
    }

    impl features::Service<MockResources> for Registry {
        fn publish(
            &mut self,
            _frame: &MockResources,
            feature: Str<'_, { ipc::wire::FIELD_DATA_MAX }>,
        ) -> Result<(), FeaturesError> {
            self.published.push(feature.as_str().into());
            Ok(())
        }
    }

    #[test]
    fn a_feature_longer_than_the_limit_reaches_the_registry() {
        let (client_end, server_end) = ipc_test::pair();
        let client = features::Client::new(client_end);
        let long = "f".repeat(FEATURE_MAX_BYTES + 1);
        let mut registry = Registry::default();
        thread::scope(|scope| {
            scope.spawn(|| {
                server_end
                    .serve(|frame| features::dispatch(&mut registry, frame))
                    .expect("dispatch ok");
            });
            let feature = Str::new(&long).expect("field bound");
            assert_eq!(client.publish(feature), Ok(()));
        });
        assert_eq!(registry.published, [long]);
    }
}
