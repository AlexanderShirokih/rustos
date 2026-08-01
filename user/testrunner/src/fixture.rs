//! Клиентская сторона `port-fixture`: спавн потомка из userland-образа и
//! запрос-ответ по его bootstrap-порту.

use bootstrap::BootstrapClient;
use runtime::{MemoryRegion, OwnedHandle, Port, PortTransport, Process, Resource, Result, Timeout};
use userland::{EntryView, SegmentView, USERLAND_PAGE_SIZE};
use userland_image::decode;
use userland_loader::load_entry;

use crate::{fixture_protocol::encode_request, ipc_raw};

/// Бюджет одного запроса к фикстуре: мёртвый потомок отказывает сразу, поэтому
/// конечный тайм-аут ловит только зависшего.
const REQUEST_TIMEOUT_NS: u64 = 5_000_000_000;
/// Бюджет ожидания завершения потомка.
pub const JOIN_TIMEOUT_NS: u64 = 5_000_000_000;

/// Имя фикстуры в userland-образе.
const FIXTURE_ENTRY: &str = "/boot/port-fixture";

/// Значок сессии родителя у фикстуры.
const FIXTURE_SESSION_BADGE: u64 = 1;

/// Метеринг спавнимой фикстуры.
pub enum Metering<'a> {
    /// Ресурс родителя.
    Inherited,
    /// Отделённый бюджет: страницы сегментов образа плюс запас на собственные
    /// аллокации фикстуры.
    SplitWithSlack(u64),
    /// Готовый ресурс вызывающего: права капы потомка задаёт он.
    Provided(&'a Resource),
}

/// Запущенный потомок-фикстура и клиентская сторона его порта.
pub struct Fixture {
    process: Process,
    port: Port,
    /// Отделённый под потомка бюджет. `None` - потомок метерится ресурсом
    /// родителя.
    budget_pages: Option<u64>,
}

impl Fixture {
    /// Спавнит фикстуру на метеринг-ресурсе родителя.
    pub fn spawn() -> Self {
        Self::spawn_metered_by(&Metering::Inherited).expect("spawn port fixture")
    }

    /// Спавнит фикстуру на отделённом от родителя бюджете: страницы сегментов
    /// её образа плюс `slack_pages` на её собственные аллокации.
    pub fn spawn_with_slack(slack_pages: u64) -> Self {
        Self::try_spawn_with_slack(slack_pages).expect("spawn port fixture")
    }

    /// То же, что [`spawn_with_slack`](Self::spawn_with_slack), но отказ
    /// загрузки образа возвращается вызывающему.
    pub fn try_spawn_with_slack(slack_pages: u64) -> Result<Self> {
        Self::spawn_metered_by(&Metering::SplitWithSlack(slack_pages))
    }

    /// Спавнит фикстуру на ресурсе вызывающего.
    pub fn spawn_on(resource: &Resource) -> Self {
        Self::spawn_metered_by(&Metering::Provided(resource)).expect("spawn port fixture")
    }

    /// Отделённый под потомка бюджет в страницах.
    pub fn budget_pages(&self) -> u64 {
        self.budget_pages
            .expect("fixture spawned on a split budget")
    }

    fn spawn_metered_by(metering: &Metering<'_>) -> Result<Self> {
        let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));
        let cap = client
            .acquire_userland_image()
            .expect("acquire_userland_image call")
            .expect("vend ok");

        let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
        let mapping = region.map_full_read_only().expect("map image read-only");
        let image = decode(mapping.as_bytes()).expect("decode acquired image");
        let program = image
            .entry(FIXTURE_ENTRY)
            .expect("image carries the port fixture")
            .program()
            .expect("fixture record carries a program");

        let service = Port::create_service(FIXTURE_SESSION_BADGE).expect("port create");

        let mut budget_pages = None;
        let own_resource;
        // Капа-источник производного ресурса живёт до конца загрузки: её закрытие отзывает
        // производную капу раньше, чем та оплатит сегменты.
        let split_parent;
        let resource = match metering {
            Metering::Provided(resource) => *resource,
            Metering::Inherited => {
                own_resource = Resource::self_resource().expect("self metering resource");
                &own_resource
            }
            Metering::SplitWithSlack(slack_pages) => {
                let pages = image_pages(&program) + slack_pages;
                budget_pages = Some(pages);
                split_parent = Resource::self_resource().expect("self metering resource");
                own_resource = split_parent.split(pages).expect("split the child budget");
                &own_resource
            }
        };
        let process = load_entry(&program, resource, service.server)?;

        Ok(Self {
            process,
            port: service.client,
            budget_pages,
        })
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

// Страницы, которых стоит образ программы: её сегменты. Стек потомка оплачивается тем же
// ресурсом сверх этой оценки.
fn image_pages<'a, E: EntryView<'a>>(program: &E) -> u64 {
    program
        .segments()
        .map(|segment| segment.mem_size().div_ceil(USERLAND_PAGE_SIZE))
        .sum()
}
