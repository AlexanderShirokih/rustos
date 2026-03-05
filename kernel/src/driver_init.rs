extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};

use drivers_common::{CapabilityStoreMut, Driver, DriverRunError, RuntimeDriverRegistry};
use klog::info;

pub struct PendingDriver {
    pub name: &'static str,
    pub driver: Box<dyn Driver>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedEntry {
    pub driver: &'static str,
    pub capability: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitSchedulerError {
    Unresolved {
        entries: Vec<UnresolvedEntry>,
    },
    Fatal {
        driver: &'static str,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitSummary {
    pub passes: usize,
    pub initialized: usize,
    pub deferred_left: usize,
}

pub fn run_retry_passes(
    mut pending: Vec<PendingDriver>,
    caps: &mut dyn CapabilityStoreMut,
    registry: &mut RuntimeDriverRegistry,
) -> Result<InitSummary, InitSchedulerError> {
    let mut passes = 0usize;
    let mut initialized = 0usize;

    while !pending.is_empty() {
        passes += 1;

        let mut progress = false;
        let mut pending_next = Vec::new();
        let mut unresolved_entries = Vec::new();

        for mut pending_driver in pending.into_iter() {
            match pending_driver.driver.run(caps) {
                Ok(()) => {
                    progress = true;
                    initialized += 1;

                    info!("Running driver {}. OK", pending_driver.name);

                    registry.insert(pending_driver.name, pending_driver.driver);
                }

                Err(DriverRunError::MissingCapability { capability }) => {
                    unresolved_entries.push(UnresolvedEntry {
                        driver: pending_driver.name,
                        capability,
                    });
                    pending_next.push(pending_driver);
                }

                Err(DriverRunError::Fatal(reason)) => {
                    info!("Running driver {}. Fatal: {}", pending_driver.name, reason);

                    return Err(InitSchedulerError::Fatal {
                        driver: pending_driver.name,
                        reason,
                    });
                }
            }
        }

        if !pending_next.is_empty() && !progress {
            return Err(InitSchedulerError::Unresolved {
                entries: unresolved_entries,
            });
        }

        pending = pending_next;
    }

    Ok(InitSummary {
        passes,
        initialized,
        deferred_left: pending.len(),
    })
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, sync::Arc, vec};
    use core::{
        marker::PhantomData,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use drivers_common::{
        Capabilities, CapabilityStoreExt, CapabilityStoreMut, CapabilityStoreMutExt,
        DriverRunError,
        services::{
            Service,
            interrupts::{InterruptsService, IrqBinding, IrqBound, IrqRegistrationError},
        },
    };
    use spin::Mutex;

    use super::*;

    struct FooCap;

    struct TestInterruptHandle;

    impl InterruptsService for TestInterruptHandle {
        fn enable(&self) {}

        fn disable(&self) {}

        fn bind(&self, _binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
            Err(IrqRegistrationError::Unsupported)
        }

        fn dispatch_interrupt(&self) {}
    }

    struct PublishCapDriver<T: 'static + Send + Sync> {
        name: &'static str,
        logs: Arc<Mutex<Vec<&'static str>>>,
        value: Arc<T>,
    }

    impl<T: 'static + Send + Sync> Driver for PublishCapDriver<T> {
        fn run(&mut self, caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
            self.logs.lock().push(self.name);
            caps.provide(self.value.clone())
                .map_err(|err| DriverRunError::Fatal(err.to_string()))
        }
    }

    struct RequireCapDriver<T: 'static + Send + Sync> {
        name: &'static str,
        logs: Arc<Mutex<Vec<&'static str>>>,
        _marker: PhantomData<T>,
    }

    impl<T: 'static + Send + Sync> Driver for RequireCapDriver<T> {
        fn run(&mut self, caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
            self.logs.lock().push(self.name);
            caps.require::<T>()
                .map(|_| ())
                .map_err(DriverRunError::from_capability_error)
        }
    }

    struct PublishServiceDriver<T: ?Sized + Service + 'static> {
        name: &'static str,
        logs: Arc<Mutex<Vec<&'static str>>>,
        value: Arc<T>,
    }

    impl<T: ?Sized + Service + 'static> Driver for PublishServiceDriver<T> {
        fn run(&mut self, caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
            self.logs.lock().push(self.name);
            caps.provide_service::<T>(self.value.clone())
                .map_err(|err| DriverRunError::Fatal(err.to_string()))
        }
    }

    struct RequireServiceDriver<T: ?Sized + Service + 'static> {
        name: &'static str,
        logs: Arc<Mutex<Vec<&'static str>>>,
        _marker: PhantomData<T>,
    }

    impl<T: ?Sized + Service + 'static> Driver for RequireServiceDriver<T> {
        fn run(&mut self, caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
            self.logs.lock().push(self.name);
            caps.require_service::<T>()
                .map(|_| ())
                .map_err(DriverRunError::from_capability_error)
        }
    }

    struct AlwaysMissingDriver;

    impl Driver for AlwaysMissingDriver {
        fn run(&mut self, _caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
            Err(DriverRunError::MissingCapability {
                capability: "FooCap",
            })
        }
    }

    struct FatalDriver;

    impl Driver for FatalDriver {
        fn run(&mut self, _caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
            Err(DriverRunError::Fatal("boom".to_string()))
        }
    }

    struct CountingDriver {
        calls: Arc<AtomicUsize>,
    }

    impl Driver for CountingDriver {
        fn run(&mut self, _caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct DropProbeGuard {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for DropProbeGuard {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct DropProbeDriver {
        _guard: DropProbeGuard,
    }

    impl Driver for DropProbeDriver {
        fn run(&mut self, _caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
            Ok(())
        }
    }

    fn pending(name: &'static str, driver: Box<dyn Driver>) -> PendingDriver {
        PendingDriver { name, driver }
    }

    #[test]
    fn initializes_all_without_retry() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let mut caps = Capabilities::new();
        let mut registry = RuntimeDriverRegistry::new();

        let pending = vec![
            pending(
                "publisher-1",
                Box::new(PublishCapDriver::<u64> {
                    name: "publisher-1",
                    logs: logs.clone(),
                    value: Arc::new(11),
                }),
            ),
            pending(
                "publisher-2",
                Box::new(PublishCapDriver::<u32> {
                    name: "publisher-2",
                    logs: logs.clone(),
                    value: Arc::new(22),
                }),
            ),
        ];

        let summary = run_retry_passes(pending, &mut caps, &mut registry).expect("must initialize");

        assert_eq!(summary.passes, 1);
        assert_eq!(summary.initialized, 2);
        assert_eq!(registry.len(), 2);
        assert_eq!(*logs.lock(), vec!["publisher-1", "publisher-2"]);
    }

    #[test]
    fn same_pass_visibility_when_capability_published_earlier() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let mut caps = Capabilities::new();
        let mut registry = RuntimeDriverRegistry::new();

        let pending = vec![
            pending(
                "publisher",
                Box::new(PublishCapDriver::<FooCap> {
                    name: "publisher",
                    logs: logs.clone(),
                    value: Arc::new(FooCap),
                }),
            ),
            pending(
                "consumer",
                Box::new(RequireCapDriver::<FooCap> {
                    name: "consumer",
                    logs: logs.clone(),
                    _marker: PhantomData,
                }),
            ),
        ];

        let summary = run_retry_passes(pending, &mut caps, &mut registry).expect("must initialize");

        assert_eq!(summary.passes, 1);
        assert_eq!(summary.initialized, 2);
        assert_eq!(*logs.lock(), vec!["publisher", "consumer"]);
    }

    #[test]
    fn retry_pass_resolves_dependency_when_provider_is_later() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let mut caps = Capabilities::new();
        let mut registry = RuntimeDriverRegistry::new();

        let pending = vec![
            pending(
                "consumer",
                Box::new(RequireCapDriver::<FooCap> {
                    name: "consumer",
                    logs: logs.clone(),
                    _marker: PhantomData,
                }),
            ),
            pending(
                "publisher",
                Box::new(PublishCapDriver::<FooCap> {
                    name: "publisher",
                    logs: logs.clone(),
                    value: Arc::new(FooCap),
                }),
            ),
        ];

        let summary = run_retry_passes(pending, &mut caps, &mut registry).expect("must initialize");

        assert_eq!(summary.passes, 2);
        assert_eq!(summary.initialized, 2);
        assert_eq!(*logs.lock(), vec!["consumer", "publisher", "consumer"]);
    }

    #[test]
    fn successful_driver_is_not_retried_on_next_passes() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut caps = Capabilities::new();
        let mut registry = RuntimeDriverRegistry::new();

        let pending = vec![
            pending(
                "counting-driver",
                Box::new(CountingDriver {
                    calls: calls.clone(),
                }),
            ),
            pending(
                "consumer",
                Box::new(RequireCapDriver::<FooCap> {
                    name: "consumer",
                    logs: Arc::new(Mutex::new(Vec::new())),
                    _marker: PhantomData,
                }),
            ),
            pending(
                "publisher",
                Box::new(PublishCapDriver::<FooCap> {
                    name: "publisher",
                    logs: Arc::new(Mutex::new(Vec::new())),
                    value: Arc::new(FooCap),
                }),
            ),
        ];

        let summary = run_retry_passes(pending, &mut caps, &mut registry).expect("must initialize");

        assert_eq!(summary.passes, 2);
        assert_eq!(summary.initialized, 3);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "successful driver must run exactly once"
        );
    }

    #[test]
    fn retry_pass_resolves_interrupt_controller_dependency() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let mut caps = Capabilities::new();
        let mut registry = RuntimeDriverRegistry::new();

        let controller: Arc<dyn InterruptsService> = Arc::new(TestInterruptHandle);

        let pending = vec![
            pending(
                "irq-consumer",
                Box::new(RequireServiceDriver::<dyn InterruptsService> {
                    name: "irq-consumer",
                    logs: logs.clone(),
                    _marker: PhantomData,
                }),
            ),
            pending(
                "gic-provider",
                Box::new(PublishServiceDriver::<dyn InterruptsService> {
                    name: "gic-provider",
                    logs: logs.clone(),
                    value: controller,
                }),
            ),
        ];

        let summary = run_retry_passes(pending, &mut caps, &mut registry).expect("must initialize");

        assert_eq!(summary.passes, 2);
        assert_eq!(summary.initialized, 2);
        assert_eq!(
            *logs.lock(),
            vec!["irq-consumer", "gic-provider", "irq-consumer"]
        );
    }

    #[test]
    fn fails_when_no_progress_with_unresolved_dependencies() {
        let mut caps = Capabilities::new();
        let mut registry = RuntimeDriverRegistry::new();
        let pending = vec![pending("missing-driver", Box::new(AlwaysMissingDriver))];

        let err = run_retry_passes(pending, &mut caps, &mut registry).expect_err("must fail");
        assert_eq!(
            err,
            InitSchedulerError::Unresolved {
                entries: vec![UnresolvedEntry {
                    driver: "missing-driver",
                    capability: "FooCap",
                }],
            }
        );
    }

    #[test]
    fn stops_immediately_on_fatal_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut caps = Capabilities::new();
        let mut registry = RuntimeDriverRegistry::new();

        let pending = vec![
            pending("fatal-driver", Box::new(FatalDriver)),
            pending(
                "counting-driver",
                Box::new(CountingDriver {
                    calls: calls.clone(),
                }),
            ),
        ];

        let err = run_retry_passes(pending, &mut caps, &mut registry).expect_err("must fail");
        assert_eq!(
            err,
            InitSchedulerError::Fatal {
                driver: "fatal-driver",
                reason: "boom".to_string(),
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn duplicate_publish_is_reported_as_fatal() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let mut caps = Capabilities::new();
        let mut registry = RuntimeDriverRegistry::new();

        let pending = vec![
            pending(
                "publisher-a",
                Box::new(PublishCapDriver::<FooCap> {
                    name: "publisher-a",
                    logs: logs.clone(),
                    value: Arc::new(FooCap),
                }),
            ),
            pending(
                "publisher-b",
                Box::new(PublishCapDriver::<FooCap> {
                    name: "publisher-b",
                    logs: logs.clone(),
                    value: Arc::new(FooCap),
                }),
            ),
        ];

        let err = run_retry_passes(pending, &mut caps, &mut registry).expect_err("must fail");

        match err {
            InitSchedulerError::Fatal { driver, reason } => {
                assert_eq!(driver, "publisher-b");
                assert!(reason.contains("already registered"));
            }
            _ => panic!("unexpected scheduler error"),
        }
    }

    #[test]
    fn registry_keeps_drivers_alive_for_raii() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut caps = Capabilities::new();
        let mut registry = RuntimeDriverRegistry::new();

        let pending = vec![pending(
            "drop-probe",
            Box::new(DropProbeDriver {
                _guard: DropProbeGuard {
                    drops: drops.clone(),
                },
            }),
        )];

        let summary = run_retry_passes(pending, &mut caps, &mut registry).expect("must initialize");
        assert_eq!(summary.initialized, 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        drop(registry);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
