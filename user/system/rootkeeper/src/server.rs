use alloc::{rc::Rc, sync::Arc};
use core::{future::Future, task::Poll};

use ::component_startup::{SessionPort, serve_calls};
use bootstrap_abi::{FeaturesError, ImageError, features, image};
use component_abi::{QueryAttributes, SessionError, SessionRequirement, component_startup};
use executor::Executor;
use ipc::{
    Incoming, IntoWireValue,
    wire::{Cap, IpcError},
};
use kernel_objects::{Delivery, IssuedHandle, kind};
use log_abi::{JournalError, LogRing, journal};
use runtime::Mutex;

use crate::system::{Holder, RootSystem};

type Shared = Arc<Mutex<RootSystem<'static>>>;
type Debt = IssuedHandle<kind::Reply>;

struct Control {
    system: Shared,
}

struct NextWaiting<'a> {
    control: &'a Control,
    badge: u64,
    key: Arc<()>,
}

impl Control {
    fn serve(
        control: &Rc<Self>,
        incoming: Incoming<'_, Delivery>,
    ) -> Result<Option<impl Future<Output = Result<(), IpcError>> + use<>>, IpcError> {
        let caller = control.system.lock().caller(incoming.resources.badge);

        let served = match caller {
            Some(Holder::Image(_)) => image::dispatch(&**control, incoming).map(|()| None),
            Some(Holder::Features(_)) => features::dispatch(&**control, incoming).map(|()| None),
            Some(Holder::Journal(_)) => journal::dispatch(&**control, incoming).map(|()| None),
            _ => component_startup::serve(control, incoming),
        };

        match served {
            Ok(Some(call)) => {
                let control = control.clone();
                Ok(Some(async move {
                    let result = call.await;
                    control.system.lock().notify();
                    result
                }))
            }
            served => {
                control.system.lock().notify();
                served.map(|_| None)
            }
        }
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl component_startup::Service<Delivery> for Control {
    fn session(
        &self,
        frame: &Delivery,
        requirement: SessionRequirement<'_>,
        attributes: QueryAttributes<'_>,
        reply: component_startup::session::Reply<Debt>,
    ) {
        self.system
            .lock()
            .request_session(frame, requirement, attributes, reply);
    }

    async fn accept(
        &self,
        frame: &Delivery,
        export: &str,
        reply: component_startup::accept::Reply<Debt>,
    ) {
        let begun = self.system.lock().begin_accept(frame.badge, export);
        let accepted = match begun {
            Ok(waiting) => waiting.await,
            Err(error) => Err(error),
        };
        let (answer, receipt) = match accepted {
            Ok((request, receipt)) => (Ok(request), Some(receipt)),
            Err(error) => (Err(error), None),
        };

        if let Err(returned) = reply.send(answer)
            && let Some(receipt) = receipt
            && let Ok(Ok(request)) = returned.into_value()
            && self.system.lock().has_owner(receipt.importer())
        {
            receipt.restore(request);
        }
    }

    fn all(
        &self,
        frame: &Delivery,
        requirement: SessionRequirement<'_>,
        attributes: QueryAttributes<'_>,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::Port>, SessionError>> {
        self.system
            .lock()
            .all_exports(frame, requirement, attributes)
    }

    fn watch(
        &self,
        frame: &Delivery,
        requirement: SessionRequirement<'_>,
        attributes: QueryAttributes<'_>,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::Port>, SessionError>> {
        self.system
            .lock()
            .watch_exports(frame, requirement, attributes)
    }

    async fn next(&self, frame: &Delivery, reply: component_startup::next::Reply<Debt>) {
        let key = Arc::new(());
        let begun = self.system.lock().begin_next(frame.badge, key.clone());
        let (result, receipt) = match begun {
            Ok(()) => {
                let waiting = NextWaiting {
                    control: self,
                    badge: frame.badge,
                    key,
                };

                core::future::poll_fn(|cx| {
                    self.system.lock().poll_next(frame.badge, &waiting.key, cx)
                })
                .await
            }
            Err(error) => (Err(error), None),
        };

        let delivered = reply.send(result).is_ok();
        if let Some(receipt) = receipt {
            self.system.lock().finish_next(&receipt, delivered);
        }
    }

    fn open(&self, frame: &Delivery, reply: component_startup::open::Reply<Debt>) {
        self.system.lock().open_found(frame, reply);
    }
}

impl features::Service<Delivery> for Control {
    fn publish(&self, frame: &Delivery, feature: &str) -> Result<(), FeaturesError> {
        self.system.lock().publish(frame, feature)
    }
}

impl image::Service<Delivery> for Control {
    fn region(
        &self,
        frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, ImageError>> {
        self.system.lock().region(frame)
    }
}

impl journal::Service<Delivery> for Control {
    fn subscribe(
        &self,
        frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<LogRing, JournalError>> {
        self.system.lock().subscribe(frame)
    }
}

impl Drop for NextWaiting<'_> {
    fn drop(&mut self) {
        self.control
            .system
            .lock()
            .cancel_next(self.badge, &self.key);
    }
}

/// Обслуживает стартовые сессии и сроки композиции задачами одного executor.
/// `system` удерживает образ процесса. Уход значка отменяет его вызовы до снятия записей.
pub fn serve(executor: &Rc<Executor>, system: Shared) -> impl Future<Output = ()> + 'static {
    let sessions: Arc<SessionPort> = system.lock().sessions();
    let control = Rc::new(Control {
        system: system.clone(),
    });

    let calls = control.clone();
    let gone = system.clone();
    let served = serve_calls(
        executor,
        sessions,
        move |frame| Control::serve(&calls, frame),
        move |badge| gone.lock().gone(badge),
    );

    async move {
        let maintenance = maintenance(&system);
        let mut maintenance = core::pin::pin!(maintenance);
        let mut served = core::pin::pin!(served);

        core::future::poll_fn(|cx| {
            if let Poll::Ready(error) = maintenance.as_mut().poll(cx) {
                system
                    .lock()
                    .log(format_args!("composition wait failed: {error:?}"));
                return Poll::Ready(());
            }

            if let Poll::Ready(error) = served.as_mut().poll(cx) {
                system.lock().log(format_args!("startup stopped: {error}"));
                return Poll::Ready(());
            }

            Poll::Pending
        })
        .await;
    }
}

async fn maintenance(system: &Shared) -> Result<(), kernel_objects::Error> {
    loop {
        let (changed, timeout) = system.lock().maintenance()?;
        if let Some(result) = executor::timeout(timeout, executor::wait(changed)).await {
            result?;
        }
    }
}
