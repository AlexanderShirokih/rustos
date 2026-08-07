//! E2E проверка `ResourceSplit` из EL0: выделенный бюджет отделён от
//! исходного, его остаток возвращается закрытием производного ресурса, а
//! потомок, стартованный с явным `resource_h`, метерится именно им.

use kernel_tests::kernel_test;
use runtime::{MemoryAccess, Priority, Process, Resource, Signal, ThreadEntry};
use syscall::{Rights, SyscallError};
use userland::USERLAND_PAGE_SIZE;

use crate::{
    fixture::Fixture,
    fixture_protocol::{
        OP_ALLOCATE_PAGES, OP_DUPLICATE_TRANSFER_CAP, OP_EXIT, REPLY_ACCESS_DENIED,
        REPLY_BUDGET_EXHAUSTED, REPLY_OK,
    },
};

const CHILD_PAGES: u64 = 2;

const DEPTH_CEILING: usize = 8;

const FIXTURE_SLACK_PAGES: u64 = 64;

const OVERSIZE_FACTOR: u64 = 4;

#[kernel_test]
fn split_chain_depth_is_capped() {
    let mut held = alloc::vec::Vec::new();
    held.push(Resource::self_resource().expect("self metering resource"));
    let denied = loop {
        // Отказ обязан наступить не позже потолка, на какой бы глубине self-ресурс ни стоял.
        kernel_tests::kassert!(held.len() <= DEPTH_CEILING + 1);
        let leaf = held.last().expect("chain is non-empty");
        match leaf.split(1) {
            Ok(next) => held.push(next),
            Err(error) => break error,
        }
    };
    kernel_tests::kassert!(matches!(
        denied,
        runtime::Error::Syscall(SyscallError::ResourceExhausted)
    ));

    // Отказ по глубине не тронул бюджет листа: страница из него минтится и после.
    let leaf = held.last().expect("chain is non-empty");
    let region = leaf
        .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
        .expect("leaf budget survives the refused split");
    drop(region);
}

#[kernel_test]
fn split_budget_bounds_the_derived_resource() {
    let parent = Resource::self_resource().expect("self metering resource");
    let child = parent.split(CHILD_PAGES).expect("split");

    let fits = child
        .create_virtual(CHILD_PAGES * USERLAND_PAGE_SIZE, MemoryAccess::RW)
        .expect("region within the derived budget");

    // Бюджет производного исчерпан: следующая страница отвергается независимо
    // от остатка исходного ресурса.
    let over = child.create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW);
    kernel_tests::kassert!(matches!(
        over.err(),
        Some(runtime::Error::Syscall(SyscallError::ResourceExhausted))
    ));

    // Исходный ресурс тем же запросом не ограничен.
    let from_parent = parent
        .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
        .expect("parent budget is untouched by the child exhaustion");

    drop(fits);
    drop(from_parent);
}

#[kernel_test]
fn closing_the_derived_resource_returns_its_budget() {
    let parent = Resource::self_resource().expect("self metering resource");

    // Бюджет исходного ресурса конечен: цикл сходится, только если закрытие
    // производного возвращает страницы.
    for _ in 0..64 {
        let child = parent.split(CHILD_PAGES).expect("split");
        let region = child
            .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
            .expect("region within the derived budget");
        drop(region);
        drop(child);
    }
}

#[kernel_test]
fn child_started_with_a_resource_is_metered_by_it() {
    let metered = Fixture::spawn_with_slack(FIXTURE_SLACK_PAGES);
    let oversized = metered.budget_pages() * OVERSIZE_FACTOR;

    // Потомок на ресурсе родителя запрос проводит: он упирается в корневой
    // бюджет, которому такой размер по силам.
    let inherited = Fixture::spawn();
    kernel_tests::kassert_eq!(
        inherited.request_status(OP_ALLOCATE_PAGES, oversized, &[]),
        REPLY_OK
    );
    kernel_tests::kassert_eq!(inherited.request_status(OP_EXIT, 0, &[]), REPLY_OK);

    // Тот же запрос у потомка на отделённом бюджете отвергается: метеринг идёт
    // по переданному ресурсу, минуя корневой.
    kernel_tests::kassert_eq!(
        metered.request_status(OP_ALLOCATE_PAGES, oversized, &[]),
        REPLY_BUDGET_EXHAUSTED
    );
    kernel_tests::kassert_eq!(metered.request_status(OP_EXIT, 0, &[]), REPLY_OK);
}

#[kernel_test]
fn child_stack_is_charged_to_the_image_budget() {
    // Бюджет ровно на сегменты образа: стек - такой же сегмент, оплатить его
    // нечем, и загрузка отказывает вместо того, чтобы взять фреймы мимо учёта.
    let denied = Fixture::try_spawn_with_slack(0);
    kernel_tests::kassert_eq!(
        denied.err(),
        Some(runtime::Error::Syscall(SyscallError::ResourceExhausted))
    );
}

#[kernel_test]
fn starting_a_child_on_a_write_only_resource_is_denied() {
    let resource = Resource::self_resource().expect("self metering resource");
    let write_only = Resource::from_handle(
        resource
            .handle()
            .duplicate(Rights::WRITE, None)
            .expect("write-only resource capability"),
    );
    let process = Process::create("resource-rights").expect("process create");
    let bootstrap = Signal::create().expect("signal create").into_handle();

    let entry = ThreadEntry {
        entry_pc: 0,
        user_sp: 0,
        arg: 0,
        priority: Priority::new(0),
    };
    // Отдача ресурса кладёт капу на него в чужую таблицу: гейт тот же, что у
    // деривации `ResourceSplit`.
    let error = process.start(entry, bootstrap, Some(&write_only)).err();
    kernel_tests::kassert!(matches!(
        error,
        Some(runtime::Error::Syscall(SyscallError::AccessDenied))
    ));

    // Контроль: тот же старт с полноправным ресурсом доходит до проверки
    // состояния процесса и отказывает другим кодом. Разные исходы на одном
    // вызове и показывают, что `AccessDenied` пришёл от прав на `resource_h`.
    let bootstrap = Signal::create().expect("signal create").into_handle();
    let error = process.start(entry, bootstrap, Some(&resource)).err();
    kernel_tests::kassert!(matches!(
        error,
        Some(runtime::Error::Syscall(SyscallError::WrongType))
    ));
}

#[kernel_test]
fn granted_resource_rights_shrink_to_the_passing_handle() {
    // Ресурс отдан хэндлом без `TRANSFER`: капа, которую потомок снимет через
    // `ProcessResourceSelf`, не шире прав этого хэндла, и разослать её потомок не может.
    let parent = Resource::self_resource().expect("self metering resource");
    let untransferable = Resource::from_handle(
        parent
            .handle()
            .duplicate(Rights::WRITE | Rights::DUPLICATE, None)
            .expect("resource capability without TRANSFER"),
    );
    let child = Fixture::spawn_on(&untransferable);
    kernel_tests::kassert_eq!(
        child.request_status(OP_DUPLICATE_TRANSFER_CAP, 0, &[]),
        REPLY_ACCESS_DENIED
    );
    kernel_tests::kassert_eq!(child.request_status(OP_EXIT, 0, &[]), REPLY_OK);

    // Контроль: у потомка на полноправном ресурсе та же проба проходит.
    let entitled = Fixture::spawn();
    kernel_tests::kassert_eq!(
        entitled.request_status(OP_DUPLICATE_TRANSFER_CAP, 0, &[]),
        REPLY_OK
    );
    kernel_tests::kassert_eq!(entitled.request_status(OP_EXIT, 0, &[]), REPLY_OK);
}

#[kernel_test]
fn split_of_zero_pages_is_rejected() {
    let parent = Resource::self_resource().expect("self metering resource");

    kernel_tests::kassert!(matches!(
        parent.split(0).err(),
        Some(runtime::Error::Syscall(SyscallError::InvalidArgument))
    ));
}
