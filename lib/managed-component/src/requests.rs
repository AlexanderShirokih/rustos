use alloc::{string::String, sync::Arc, vec::Vec};
use core::{
    future::Future,
    mem::size_of,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use component_abi::{AcceptError, AttributeSet, SessionError, SessionRequest, component_startup};
use kernel_objects::{IssuedHandle, kind};
use runtime::Mutex;

use crate::{Held, IMPORTER_REQUESTS_PER_EXPORT_MAX};

type ImporterDebt = component_startup::session::Reply<IssuedHandle<kind::Reply>>;
type Request = SessionRequest<ImporterDebt>;

pub(crate) struct Requests<C>(Arc<Mutex<Queue<C>>>);

struct Queue<C> {
    requests: Vec<PendingRequest<C>>,
    accepts: Vec<PendingAccept<C>>,
    next_sequence: u64,
}

struct PendingRequest<C> {
    origin: Origin<C>,
    request: Request,
}

struct Origin<C> {
    sequence: u64,
    provider: C,
    importer: u64,
    requirement: String,
    protocol: String,
}

struct PendingAccept<C> {
    provider: C,
    badge: u64,
    protocol: String,
    key: Arc<()>,
    waker: Option<Waker>,
}

/// Ожидание заявки провайдером. Разрушение снимает регистрацию ожидания, оставляя
/// импортёрские заявки в очереди. Замена ожидания той же сессии и протокола даёт `NoSpace`.
pub struct Accept<C> {
    queue: Arc<Mutex<Queue<C>>>,
    key: Arc<()>,
}

/// Место заявки в очереди до отправки провайдеру. Успешная отправка освобождает значение,
/// отказ возвращает заявку через [`Self::restore`].
pub struct ReturnRequest<C> {
    queue: Arc<Mutex<Queue<C>>>,
    origin: Origin<C>,
}

impl<C: Copy + Eq> Requests<C> {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(Queue {
            requests: Vec::new(),
            accepts: Vec::new(),
            next_sequence: 0,
        })))
    }

    pub(crate) const fn request_bytes(requirement: &str, protocol: &str) -> usize {
        size_of::<PendingRequest<C>>() + requirement.len() + protocol.len()
    }

    pub(crate) fn request(
        &self,
        provider: C,
        importer: u64,
        requirement: &str,
        protocol: &str,
        attributes: AttributeSet,
        reply: ImporterDebt,
    ) {
        let request = SessionRequest { reply, attributes };
        let Some((requirement, protocol_name)) = owned(requirement).zip(owned(protocol)) else {
            refuse(request, SessionError::NoSpace);
            return;
        };

        let inserted = {
            let mut queue = self.0.lock();
            match queue.next_sequence.checked_add(1) {
                Some(sequence) if queue.requests.try_reserve(1).is_ok() => {
                    queue.next_sequence = sequence;
                    let origin = Origin {
                        sequence,
                        provider,
                        importer,
                        requirement,
                        protocol: protocol_name,
                    };

                    let evicted = queue.insert(PendingRequest { origin, request });
                    Ok((evicted, queue.waker(provider, protocol)))
                }
                _ => Err(request),
            }
        };

        match inserted {
            Ok((evicted, waker)) => {
                if let Some(evicted) = evicted {
                    refuse(evicted.request, SessionError::NoSpace);
                }

                if let Some(waker) = waker {
                    waker.wake();
                }
            }

            Err(request) => refuse(request, SessionError::NoSpace),
        }
    }

    pub(crate) fn accept(
        &self,
        provider: C,
        badge: u64,
        protocol: &str,
    ) -> Result<Accept<C>, AcceptError> {
        let protocol = owned(protocol).ok_or(AcceptError::NoSpace)?;
        let key = Arc::new(());

        let replaced = {
            let mut queue = self.0.lock();
            let existing = queue
                .accepts
                .iter()
                .position(|accept| accept.badge == badge && accept.protocol == protocol);
            let accept = PendingAccept {
                provider,
                badge,
                protocol,
                key: key.clone(),
                waker: None,
            };

            if let Some(index) = existing {
                queue
                    .accepts
                    .get_mut(index)
                    .map(|slot| core::mem::replace(slot, accept))
            } else {
                queue
                    .accepts
                    .try_reserve(1)
                    .map_err(|_| AcceptError::NoSpace)?;
                queue.accepts.push(accept);
                None
            }
        };

        if let Some(waker) = replaced.and_then(|accept| accept.waker) {
            waker.wake();
        }

        Ok(Accept {
            queue: self.0.clone(),
            key,
        })
    }

    pub(crate) fn release(&self, owner: u64) {
        let wakers: Vec<_> = {
            let mut queue = self.0.lock();
            queue
                .requests
                .retain(|request| request.origin.importer != owner);
            queue
                .accepts
                .extract_if(.., |accept| accept.badge == owner)
                .filter_map(|accept| accept.waker)
                .collect()
        };

        for waker in wakers {
            waker.wake();
        }
    }

    pub(crate) fn refuse_pending(&self, provider: C) {
        let refused: Vec<_> = self
            .0
            .lock()
            .requests
            .extract_if(.., |request| request.origin.provider == provider)
            .collect();

        for pending in refused {
            refuse(pending.request, SessionError::Unavailable);
        }
    }

    pub(crate) fn has_pending(&self, provider: C) -> bool {
        self.0
            .lock()
            .requests
            .iter()
            .any(|request| request.origin.provider == provider)
    }

    pub(crate) fn held_where(&self, owned_by: impl Fn(u64) -> bool) -> Held {
        let queue = self.0.lock();
        let mut held = Held::default();

        for request in queue
            .requests
            .iter()
            .filter(|request| owned_by(request.origin.importer))
        {
            held.states += 1;
            held.bytes += size_of::<PendingRequest<C>>()
                + request.origin.protocol.capacity()
                + request.origin.requirement.capacity();
        }

        held
    }
}

impl<C: Copy + Eq> Queue<C> {
    fn insert(&mut self, request: PendingRequest<C>) -> Option<PendingRequest<C>> {
        let same_key = |pending: &PendingRequest<C>| {
            pending.origin.importer == request.origin.importer
                && pending.origin.requirement == request.origin.requirement
                && pending.origin.provider == request.origin.provider
                && pending.origin.protocol == request.origin.protocol
        };

        let count = self
            .requests
            .iter()
            .filter(|request| same_key(request))
            .count();
        let oldest = self.requests.iter().position(same_key);

        let position = self
            .requests
            .partition_point(|pending| pending.origin.sequence < request.origin.sequence);

        self.requests.insert(position, request);
        if count >= IMPORTER_REQUESTS_PER_EXPORT_MAX {
            let oldest = oldest.map_or(position, |oldest| oldest.min(position));
            Some(self.requests.remove(oldest))
        } else {
            None
        }
    }

    fn waker(&mut self, provider: C, protocol: &str) -> Option<Waker> {
        self.accepts
            .iter_mut()
            .filter(|accept| accept.provider == provider && accept.protocol == protocol)
            .find_map(|accept| accept.waker.take())
    }
}

impl<C: Copy + Eq> Future for Accept<C> {
    type Output = Result<(Request, ReturnRequest<C>), AcceptError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut queue = self.queue.lock();
        let Some(index) = queue
            .accepts
            .iter()
            .position(|accept| Arc::ptr_eq(&accept.key, &self.key))
        else {
            return Poll::Ready(Err(AcceptError::NoSpace));
        };

        let Some(accept) = queue.accepts.get(index) else {
            return Poll::Ready(Err(AcceptError::NoSpace));
        };

        let request = queue.requests.iter().position(|pending| {
            pending.origin.provider == accept.provider && pending.origin.protocol == accept.protocol
        });
        if let Some(request) = request {
            queue.accepts.remove(index);
            let PendingRequest { origin, request } = queue.requests.remove(request);
            return Poll::Ready(Ok((
                request,
                ReturnRequest {
                    queue: self.queue.clone(),
                    origin,
                },
            )));
        }

        if let Some(accept) = queue.accepts.get_mut(index) {
            accept.waker = Some(cx.waker().clone());
        }

        Poll::Pending
    }
}

impl<C> Drop for Accept<C> {
    fn drop(&mut self) {
        let wakers: Vec<_> = {
            let mut queue = self.queue.lock();
            let Some(index) = queue
                .accepts
                .iter()
                .position(|accept| Arc::ptr_eq(&accept.key, &self.key))
            else {
                return;
            };

            queue.accepts.remove(index);
            if queue.requests.is_empty() {
                return;
            }
            queue
                .accepts
                .iter_mut()
                .filter_map(|accept| accept.waker.take())
                .collect()
        };

        for waker in wakers {
            waker.wake();
        }
    }
}

impl<C: Copy + Eq> ReturnRequest<C> {
    /// Значок владельца заявки. Перед восстановлением родитель проверяет, что владелец
    /// не ушёл, иначе заявку следует закрыть.
    pub const fn importer(&self) -> u64 {
        self.origin.importer
    }

    /// Возвращает недоставленную заявку в порядок поступления. Вытеснение сверх предела
    /// сохраняет правило отказа самой давней заявке.
    pub fn restore(self, request: Request) {
        let (evicted, waker) = {
            let mut queue = self.queue.lock();
            if queue.requests.try_reserve(1).is_err() {
                (
                    Some(PendingRequest {
                        origin: self.origin,
                        request,
                    }),
                    None,
                )
            } else {
                let waker = queue.waker(self.origin.provider, &self.origin.protocol);
                let evicted = queue.insert(PendingRequest {
                    origin: self.origin,
                    request,
                });
                (evicted, waker)
            }
        };

        if let Some(evicted) = evicted {
            refuse(evicted.request, SessionError::NoSpace);
        }

        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

fn refuse(request: Request, error: SessionError) {
    let _ = request
        .reply
        .send(Err::<IssuedHandle<kind::Port>, _>(error));
}

pub(crate) fn owned(text: &str) -> Option<String> {
    let mut owned = String::new();
    owned.try_reserve_exact(text.len()).ok()?;
    owned.push_str(text);

    Some(owned)
}
