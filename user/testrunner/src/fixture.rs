//! Клиентская сторона `port-fixture`: спавн потомка из userland-образа и
//! запрос-ответ по его bootstrap-порту.

use bootstrap::BootstrapClient;
use runtime::{
    MemoryRegion, OwnedHandle, Port, PortTransport, Process, Resource, Timeout, UserMemFlags,
};
use userland_image::decode;
use userland_loader::load_entry;

use crate::{fixture_protocol::encode_request, ipc_raw};

/// Бюджет одного запроса к фикстуре: мёртвый потомок отказывает сразу, поэтому
/// конечный тайм-аут ловит только зависшего.
const REQUEST_TIMEOUT_NS: u64 = 5_000_000_000;
/// Бюджет ожидания завершения потомка.
pub const JOIN_TIMEOUT_NS: u64 = 5_000_000_000;

/// Имя фикстуры в userland-образе.
const FIXTURE_ENTRY: &str = "port-fixture";

/// Значок сессии родителя у фикстуры.
const FIXTURE_SESSION_BADGE: u64 = 1;

/// Запущенный потомок-фикстура и клиентская сторона его порта.
pub struct Fixture {
    process: Process,
    port: Port,
}

impl Fixture {
    /// Спавнит фикстуру из userland-образа, отдав ей серверную сторону порта.
    pub fn spawn() -> Self {
        let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));
        let cap = client
            .acquire_userland_image()
            .expect("acquire_userland_image call")
            .expect("vend ok");

        let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
        let info = region.inspect().expect("inspect image region");
        let mapping = region
            .map(info.size_bytes, UserMemFlags::ReadOnly)
            .expect("map image read-only");
        let image = decode(mapping.as_bytes()).expect("decode acquired image");
        let entry = image
            .entry(FIXTURE_ENTRY)
            .expect("image carries the port fixture");

        let service = Port::create_service(FIXTURE_SESSION_BADGE).expect("port create");

        let resource = Resource::self_resource().expect("self metering resource");
        let process = load_entry(&entry, &resource, service.server).expect("spawn port fixture");

        Self {
            process,
            port: service.client,
        }
    }

    /// Шлёт запрос `op(arg)` с `caps` и складывает тело ответа в `out`.
    /// Возвращает полную длину ответа.
    pub fn request(&self, op: u8, arg: u64, caps: &[u32], out: &mut [u8]) -> usize {
        ipc_raw::write_frame(&encode_request(op, arg), caps);
        self.port
            .call(Timeout::from_ns(REQUEST_TIMEOUT_NS))
            .expect("fixture answers the request");
        let (len, _) = ipc_raw::read_frame(out);
        len
    }

    /// Односложный ответ фикстуры.
    pub fn request_status(&self, op: u8, arg: u64, caps: &[u32]) -> u8 {
        let mut out = [0u8; 1];
        let len = self.request(op, arg, caps, &mut out);
        kernel_tests::kassert_eq!(len, 1);
        out[0]
    }

    /// Процесс потомка.
    pub fn process(&self) -> &Process {
        &self.process
    }

    /// Закрывает клиентскую сторону порта, оставляя процесс потомка: значок
    /// сессии уходит, и потомок завершается сам.
    pub fn into_process(self) -> Process {
        self.process
    }
}
