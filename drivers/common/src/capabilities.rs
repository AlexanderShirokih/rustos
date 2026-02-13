extern crate alloc;

use crate::services::Service;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::any::{Any, TypeId, type_name};
use core::fmt::{Display, Formatter};

const UNKNOWN_CAPABILITY: &str = "<unknown>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityError {
    Missing { type_name: &'static str },
    TypeMismatch { type_name: &'static str },
    Duplicate { type_name: &'static str },
}

impl CapabilityError {
    fn missing_for<T: ?Sized + 'static>() -> Self {
        Self::Missing {
            type_name: type_name::<T>(),
        }
    }

    fn mismatch_for<T: ?Sized + 'static>() -> Self {
        Self::TypeMismatch {
            type_name: type_name::<T>(),
        }
    }

    fn duplicate_for<T: ?Sized + 'static>() -> Self {
        Self::Duplicate {
            type_name: type_name::<T>(),
        }
    }
}

fn remap_error_for<T: ?Sized + 'static>(err: CapabilityError) -> CapabilityError {
    // Raw API хранилища работает по TypeId и не может сохранить осмысленное имя типа.
    // Восстанавливаем тот же вариант ошибки с конкретным generic-типом для диагностики.
    match err {
        CapabilityError::Missing { .. } => CapabilityError::missing_for::<T>(),
        CapabilityError::TypeMismatch { .. } => CapabilityError::mismatch_for::<T>(),
        CapabilityError::Duplicate { .. } => CapabilityError::duplicate_for::<T>(),
    }
}

impl Display for CapabilityError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            CapabilityError::Missing { type_name } => {
                write!(f, "Capability is missing: {type_name}")
            }
            CapabilityError::TypeMismatch { type_name } => {
                write!(f, "Capability has unexpected type: {type_name}")
            }
            CapabilityError::Duplicate { type_name } => {
                write!(f, "Capability is already registered: {type_name}")
            }
        }
    }
}

pub trait CapabilityStore {
    fn require_raw(&self, type_id: TypeId) -> Result<Arc<dyn Any + Send + Sync>, CapabilityError>;
}

pub trait CapabilityStoreMut: CapabilityStore {
    fn provide_raw(
        &mut self,
        type_id: TypeId,
        value: Arc<dyn Any + Send + Sync>,
    ) -> Result<(), CapabilityError>;
}

struct ServiceCapability<T: ?Sized + Service + 'static> {
    value: Arc<T>,
}

pub trait CapabilityStoreExt: CapabilityStore {
    fn require<T: 'static + Send + Sync>(&self) -> Result<Arc<T>, CapabilityError> {
        self.require_raw(TypeId::of::<T>())
            .map_err(remap_error_for::<T>)?
            .downcast::<T>()
            .map_err(|_| CapabilityError::mismatch_for::<T>())
    }

    fn require_service<T: ?Sized + Service + 'static>(
        &self,
    ) -> Result<Arc<T>, CapabilityError> {
        self.require_raw(TypeId::of::<ServiceCapability<T>>())
            .map_err(remap_error_for::<T>)?
            .downcast::<ServiceCapability<T>>()
            .map_err(|_| CapabilityError::mismatch_for::<T>())
            .map(|service| service.value.clone())
    }
}

pub trait CapabilityStoreMutExt: CapabilityStoreMut {
    fn provide<T: 'static + Send + Sync>(&mut self, value: Arc<T>) -> Result<(), CapabilityError> {
        self.provide_raw(TypeId::of::<T>(), value)
            .map_err(remap_error_for::<T>)
    }

    fn provide_service<T: ?Sized + Service + 'static>(
        &mut self,
        value: Arc<T>,
    ) -> Result<(), CapabilityError> {
        self.provide_raw(
            TypeId::of::<ServiceCapability<T>>(),
            Arc::new(ServiceCapability { value }),
        )
        .map_err(remap_error_for::<T>)
    }
}

impl<T: CapabilityStore + ?Sized> CapabilityStoreExt for T {}
impl<T: CapabilityStoreMut + ?Sized> CapabilityStoreMutExt for T {}

pub struct Capabilities {
    entries: BTreeMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl Capabilities {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilityStore for Capabilities {
    fn require_raw(&self, type_id: TypeId) -> Result<Arc<dyn Any + Send + Sync>, CapabilityError> {
        self.entries
            .get(&type_id)
            .cloned()
            .ok_or(CapabilityError::Missing {
                type_name: UNKNOWN_CAPABILITY,
            })
    }
}

impl CapabilityStoreMut for Capabilities {
    fn provide_raw(
        &mut self,
        type_id: TypeId,
        value: Arc<dyn Any + Send + Sync>,
    ) -> Result<(), CapabilityError> {
        if self.entries.contains_key(&type_id) {
            return Err(CapabilityError::Duplicate {
                type_name: UNKNOWN_CAPABILITY,
            });
        }

        self.entries.insert(type_id, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provide_and_require() {
        let mut caps = Capabilities::new();

        caps.provide::<u32>(Arc::new(42))
            .expect("failed to publish capability");

        let value = caps.require::<u32>().expect("missing capability");
        assert_eq!(*value, 42);
    }

    #[test]
    fn missing_capability() {
        let caps = Capabilities::new();

        let err = caps.require::<u64>().expect_err("must be missing");
        assert_eq!(
            err,
            CapabilityError::Missing {
                type_name: type_name::<u64>()
            }
        );
    }

    #[test]
    fn type_mismatch() {
        let mut caps = Capabilities::new();
        caps.provide_raw(TypeId::of::<u64>(), Arc::new(10u32))
            .expect("failed to publish raw capability");

        let err = caps.require::<u64>().expect_err("must be mismatched");
        assert_eq!(
            err,
            CapabilityError::TypeMismatch {
                type_name: type_name::<u64>()
            }
        );
    }

    #[test]
    fn duplicate_capability() {
        let mut caps = Capabilities::new();

        let first = caps.provide::<u64>(Arc::new(10));
        let second = caps.provide::<u64>(Arc::new(11));

        assert!(first.is_ok());
        assert_eq!(
            second,
            Err(CapabilityError::Duplicate {
                type_name: type_name::<u64>()
            })
        );
    }

    trait TestService: Service {
        fn value(&self) -> u32;
    }

    struct TestServiceImpl(u32);

    impl TestService for TestServiceImpl {
        fn value(&self) -> u32 {
            self.0
        }
    }

    #[test]
    fn provide_and_require_service_capability() {
        let mut caps = Capabilities::new();
        let service: Arc<dyn TestService> = Arc::new(TestServiceImpl(7));

        caps.provide_service::<dyn TestService>(service.clone())
            .expect("failed to publish service capability");

        let resolved = caps
            .require_service::<dyn TestService>()
            .expect("missing service capability");

        assert_eq!(resolved.value(), 7);
        assert!(Arc::ptr_eq(&service, &resolved));
    }

    #[test]
    fn duplicate_service_capability() {
        let mut caps = Capabilities::new();
        let first: Arc<dyn TestService> = Arc::new(TestServiceImpl(7));
        let second: Arc<dyn TestService> = Arc::new(TestServiceImpl(8));

        let first_result = caps.provide_service::<dyn TestService>(first);
        let second_result = caps.provide_service::<dyn TestService>(second);

        assert!(first_result.is_ok());
        assert_eq!(
            second_result,
            Err(CapabilityError::Duplicate {
                type_name: type_name::<dyn TestService>()
            })
        );
    }
}
