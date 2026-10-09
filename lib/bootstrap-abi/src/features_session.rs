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
        call fn publish(feature: &str) -> Result<(), FeaturesError>;
    }
}
