//! E2E проверка `ProcessLoadImage`/`ProcessStart`: parent payload в
//! userspace создаёт child, готовит образ и стартует его, после чего
//! сигналит sentinel-Event для завершения теста.

use alloc::vec;

use kobject::{EVENT_SIGNALED, Event, Handle, KObject, Rights, THREAD_TERMINATED};
use memory::{
    MemFlags,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{Priority, SchedulerServiceExt, UserProcessLaunch};
use syscall::{SyscallOp, USER_IMAGE_DESC_SIZE, USER_SEGMENT_SIZE, UserMemFlags};
use test_harness_qemu::register_test;
use test_harness_qemu_aarch64::payload::Instruction;
use userspace::{UserImage, UserSegment};

use super::user_payload::{
    B_LOOP, Reg, b_ne, cbz_x, cmp_x_imm12, mov_x, movk_x, movz_w, movz_x, str_w_imm, str_x_imm,
    svc_op, tbnz_x,
};

const PAGE_SIZE: usize = 4096;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;

/// Child VA: фиксированный low-half адрес, не пересекающийся с parent'ом.
const CHILD_CODE_VA: u64 = 0x2000_0000;
const CHILD_STACK_TOP: u64 = 0x2010_0000;
const CHILD_STACK_SIZE: u64 = PAGE_SIZE as u64;
const CHILD_USER_VM_BASE: u64 = 0x2002_0000;
const CHILD_USER_VM_SIZE: u64 = 0x10_0000;
const CHILD_EXIT_CODE: u16 = 0x0055;
// Локальная диагностика этого теста: parent payload кодирует стадию
// отказа в exit_code и подаёт отдельный failure-сигнал, чтобы вместо
// немого QEMU-timeout получить точную причину падения.
const SUCCESS_SIGNAL: u32 = EVENT_SIGNALED;
const FAILURE_SIGNAL: u32 = 1 << 1;
const CHILD_WAIT_TIMEOUT_NS: u64 = 500_000_000;

const FAIL_STAGE_REGION_CREATE: u16 = 1;
const FAIL_STAGE_REGION_MAP: u16 = 2;
const FAIL_STAGE_REGION_REMAP: u16 = 3;
const FAIL_STAGE_DESC_ALLOC: u16 = 4;
const FAIL_STAGE_PROCESS_CREATE: u16 = 5;
const FAIL_STAGE_PROCESS_LOAD_IMAGE: u16 = 6;
const FAIL_STAGE_PROCESS_START: u16 = 7;
const FAIL_STAGE_PROCESS_WAIT: u16 = 8;
const FAIL_STAGE_PROCESS_WAIT_MASK: u16 = 9;

const fn mov_x_imm(rd: Reg, imm: u64) -> [Instruction; 4] {
    [
        movz_x(rd, imm as u16, 0),
        movk_x(rd, (imm >> 16) as u16, 1),
        movk_x(rd, (imm >> 32) as u16, 2),
        movk_x(rd, (imm >> 48) as u16, 3),
    ]
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

fn patch_tbnz(words: &mut [Instruction], branch: usize, target: usize, reg: Reg, bit: u8) {
    let disp = u32::try_from(target - branch).expect("forward branch distance fits in u32");
    words[branch] = tbnz_x(reg, bit, disp);
}

fn patch_cbz(words: &mut [Instruction], branch: usize, target: usize, reg: Reg) {
    let disp = u32::try_from(target - branch).expect("forward branch distance fits in u32");
    words[branch] = cbz_x(reg, disp);
}

fn patch_b_ne(words: &mut [Instruction], branch: usize, target: usize) {
    let disp = u32::try_from(target - branch).expect("forward branch distance fits in u32");
    words[branch] = b_ne(disp);
}

fn append_failure_block(words: &mut alloc::vec::Vec<Instruction>, stage: u16) -> usize {
    let start = words.len();
    words.push(mov_x(Reg::X0, Reg::X25));
    words.push(movz_x(Reg::X1, FAILURE_SIGNAL as u16, 0));
    words.push(movz_x(Reg::X2, 0, 0));
    words.push(svc_op(SyscallOp::ObjectSignal));
    words.push(movz_x(Reg::X0, stage, 0));
    words.push(svc_op(SyscallOp::ThreadExit));
    words.push(B_LOOP);
    start
}

/// Регистры payload-а:
///  - X20: VA child-кода в parent AS.
///  - X21: VA `UserImageDescAbi`+segment в parent AS.
///  - X22: handle child-процесса.
///  - X25: sentinel Event handle (передан через bootstrap arg).
fn build_parent_payload() -> alloc::vec::Vec<u8> {
    let mut words: alloc::vec::Vec<Instruction> = alloc::vec::Vec::new();

    // X25 хранит bootstrap arg (sentinel Event handle).
    words.push(mov_x(Reg::X25, Reg::X0));

    // Создаём region для child-кода и маппим его в parent AS как RW
    // (access_mask=RWX, чтобы позже сделать remap в RX).
    words.push(movz_x(Reg::X0, 0x1000, 0));
    words.push(movz_x(Reg::X1, 0x7, 0));
    words.push(svc_op(SyscallOp::MemoryCreateVirtual));
    let fail_region_create_neg = words.len();
    words.push(tbnz_x(Reg::X0, 63, 0));
    let fail_region_create_zero = words.len();
    words.push(cbz_x(Reg::X0, 0));
    words.push(mov_x(Reg::X19, Reg::X0));

    words.push(mov_x(Reg::X0, Reg::X19));
    words.push(movz_x(Reg::X1, 0x1000, 0));
    words.push(movz_x(Reg::X2, UserMemFlags::ReadWrite as u16, 0));
    words.push(svc_op(SyscallOp::MemoryMap));
    let fail_region_map_neg = words.len();
    words.push(tbnz_x(Reg::X0, 63, 0));
    let fail_region_map_zero = words.len();
    words.push(cbz_x(Reg::X0, 0));
    words.push(mov_x(Reg::X20, Reg::X0));

    // Пишем child-инструкции по [X20]: movz w0, #CHILD_EXIT_CODE; svc
    // ThreadExit; b .
    let child_word_0 = movz_w(Reg::X0, CHILD_EXIT_CODE).word();
    let child_word_1 = svc_op(SyscallOp::ThreadExit).word();
    let child_word_2 = B_LOOP.word();
    let qword_0: u64 = (child_word_0 as u64) | ((child_word_1 as u64) << 32);
    let qword_1: u64 = child_word_2 as u64;
    words.extend_from_slice(&mov_x_imm(Reg::X1, qword_0));
    words.push(str_x_imm(Reg::X1, Reg::X20, 0));
    words.extend_from_slice(&mov_x_imm(Reg::X1, qword_1));
    words.push(str_x_imm(Reg::X1, Reg::X20, 8));

    // Поднимаем регион в RX через MemoryRemap.
    words.push(mov_x(Reg::X0, Reg::X20));
    words.push(movz_x(Reg::X1, 0x1000, 0));
    words.push(movz_x(Reg::X2, UserMemFlags::ReadExecute as u16, 0));
    words.push(svc_op(SyscallOp::MemoryRemap));
    let fail_region_remap_neg = words.len();
    words.push(tbnz_x(Reg::X0, 63, 0));

    // RW-страница под UserImageDescAbi (offset 0) + UserSegmentAbi
    // (offset 64). Обе структуры умещаются в одну страницу.
    words.push(movz_x(Reg::X0, 0x1000, 0));
    words.push(movz_x(Reg::X1, UserMemFlags::ReadWrite as u16, 0));
    words.push(svc_op(SyscallOp::MemoryAllocate));
    let fail_desc_alloc_neg = words.len();
    words.push(tbnz_x(Reg::X0, 63, 0));
    let fail_desc_alloc_zero = words.len();
    words.push(cbz_x(Reg::X0, 0));
    words.push(mov_x(Reg::X21, Reg::X0));

    // Заполняем UserImageDescAbi: version=1, segment_count=1, потом
    // segments_va, entry_va, user_stack_top/size, user_vm_base/size.
    let header: u64 = 1u64 | (1u64 << 32);
    words.extend_from_slice(&mov_x_imm(Reg::X1, header));
    words.push(str_x_imm(Reg::X1, Reg::X21, 0));
    // segments_va = X21 + 64 (через add immediate, инструкция собирается
    // вручную, т.к. helper-а на add нет).
    words.push(Instruction::raw(
        0x9100_0000 | (64u32 << 10) | (21u32 << 5) | 1u32,
    ));
    words.push(str_x_imm(Reg::X1, Reg::X21, 8));
    words.extend_from_slice(&mov_x_imm(Reg::X1, CHILD_CODE_VA));
    words.push(str_x_imm(Reg::X1, Reg::X21, 16));
    words.extend_from_slice(&mov_x_imm(Reg::X1, CHILD_STACK_TOP));
    words.push(str_x_imm(Reg::X1, Reg::X21, 24));
    words.extend_from_slice(&mov_x_imm(Reg::X1, CHILD_STACK_SIZE));
    words.push(str_x_imm(Reg::X1, Reg::X21, 32));
    words.extend_from_slice(&mov_x_imm(Reg::X1, CHILD_USER_VM_BASE));
    words.push(str_x_imm(Reg::X1, Reg::X21, 40));
    words.extend_from_slice(&mov_x_imm(Reg::X1, CHILD_USER_VM_SIZE));
    words.push(str_x_imm(Reg::X1, Reg::X21, 48));

    // Заполняем UserSegmentAbi по offset 64: region_handle, flags,
    // va_base, mapped_size, reserved.
    words.push(str_w_imm(Reg::X19, Reg::X21, 64));
    words.push(movz_w(Reg::X1, UserMemFlags::ReadExecute as u16));
    words.push(str_w_imm(Reg::X1, Reg::X21, 68));
    words.extend_from_slice(&mov_x_imm(Reg::X1, CHILD_CODE_VA));
    words.push(str_x_imm(Reg::X1, Reg::X21, 72));
    words.extend_from_slice(&mov_x_imm(Reg::X1, 0x1000));
    words.push(str_x_imm(Reg::X1, Reg::X21, 80));
    words.push(movz_x(Reg::X1, 0, 0));
    words.push(str_x_imm(Reg::X1, Reg::X21, 88));
    let _ = qword_1;

    // ProcessCreate без имени.
    words.push(movz_x(Reg::X0, 0, 0));
    words.push(movz_x(Reg::X1, 0, 0));
    words.push(svc_op(SyscallOp::ProcessCreate));
    let fail_process_create_neg = words.len();
    words.push(tbnz_x(Reg::X0, 63, 0));
    let fail_process_create_zero = words.len();
    words.push(cbz_x(Reg::X0, 0));
    words.push(mov_x(Reg::X22, Reg::X0));

    // ProcessLoadImage(child_h, desc_va, USER_IMAGE_DESC_SIZE).
    words.push(mov_x(Reg::X0, Reg::X22));
    words.push(mov_x(Reg::X1, Reg::X21));
    words.push(movz_x(Reg::X2, USER_IMAGE_DESC_SIZE as u16, 0));
    words.push(svc_op(SyscallOp::ProcessLoadImage));
    let fail_process_load_image_neg = words.len();
    words.push(tbnz_x(Reg::X0, 63, 0));

    // ProcessStart(child_h, entry, user_sp, arg=0, priority=1, 0 handles).
    words.push(mov_x(Reg::X0, Reg::X22));
    words.extend_from_slice(&mov_x_imm(Reg::X1, CHILD_CODE_VA));
    words.extend_from_slice(&mov_x_imm(Reg::X2, CHILD_STACK_TOP));
    words.push(movz_x(Reg::X3, 0, 0));
    words.push(movz_x(Reg::X4, 1, 0));
    words.push(movz_x(Reg::new(5), 0, 0));
    words.push(svc_op(SyscallOp::ProcessStart));
    let fail_process_start_neg = words.len();
    words.push(tbnz_x(Reg::X0, 63, 0));
    let fail_process_start_zero = words.len();
    words.push(cbz_x(Reg::X0, 0));

    // ObjectWaitOne(child_h, PROCESS_TERMINATED, timeout=0.5 секунды).
    words.push(mov_x(Reg::X0, Reg::X22));
    words.push(movz_x(Reg::X1, 1, 0));
    words.extend_from_slice(&mov_x_imm(Reg::X2, CHILD_WAIT_TIMEOUT_NS));
    words.push(svc_op(SyscallOp::ObjectWaitOne));
    let fail_process_wait_neg = words.len();
    words.push(tbnz_x(Reg::X0, 63, 0));
    words.push(cmp_x_imm12(Reg::X0, 1));
    let fail_process_wait_mask = words.len();
    words.push(b_ne(0));

    // ObjectSignal(sentinel_event, SUCCESS_SIGNAL, 0).
    words.push(mov_x(Reg::X0, Reg::X25));
    words.push(movz_x(Reg::X1, SUCCESS_SIGNAL as u16, 0));
    words.push(movz_x(Reg::X2, 0, 0));
    words.push(svc_op(SyscallOp::ObjectSignal));

    // ThreadExit(0).
    words.push(movz_x(Reg::X0, 0, 0));
    words.push(svc_op(SyscallOp::ThreadExit));
    words.push(B_LOOP);

    let fail_region_create = append_failure_block(&mut words, FAIL_STAGE_REGION_CREATE);
    let fail_region_map = append_failure_block(&mut words, FAIL_STAGE_REGION_MAP);
    let fail_region_remap = append_failure_block(&mut words, FAIL_STAGE_REGION_REMAP);
    let fail_desc_alloc = append_failure_block(&mut words, FAIL_STAGE_DESC_ALLOC);
    let fail_process_create = append_failure_block(&mut words, FAIL_STAGE_PROCESS_CREATE);
    let fail_process_load_image = append_failure_block(&mut words, FAIL_STAGE_PROCESS_LOAD_IMAGE);
    let fail_process_start = append_failure_block(&mut words, FAIL_STAGE_PROCESS_START);
    let fail_process_wait = append_failure_block(&mut words, FAIL_STAGE_PROCESS_WAIT);
    let fail_process_wait_bad_mask = append_failure_block(&mut words, FAIL_STAGE_PROCESS_WAIT_MASK);

    patch_tbnz(
        &mut words,
        fail_region_create_neg,
        fail_region_create,
        Reg::X0,
        63,
    );
    patch_cbz(
        &mut words,
        fail_region_create_zero,
        fail_region_create,
        Reg::X0,
    );
    patch_tbnz(
        &mut words,
        fail_region_map_neg,
        fail_region_map,
        Reg::X0,
        63,
    );
    patch_cbz(&mut words, fail_region_map_zero, fail_region_map, Reg::X0);
    patch_tbnz(
        &mut words,
        fail_region_remap_neg,
        fail_region_remap,
        Reg::X0,
        63,
    );
    patch_tbnz(
        &mut words,
        fail_desc_alloc_neg,
        fail_desc_alloc,
        Reg::X0,
        63,
    );
    patch_cbz(&mut words, fail_desc_alloc_zero, fail_desc_alloc, Reg::X0);
    patch_tbnz(
        &mut words,
        fail_process_create_neg,
        fail_process_create,
        Reg::X0,
        63,
    );
    patch_cbz(
        &mut words,
        fail_process_create_zero,
        fail_process_create,
        Reg::X0,
    );
    patch_tbnz(
        &mut words,
        fail_process_load_image_neg,
        fail_process_load_image,
        Reg::X0,
        63,
    );
    patch_tbnz(
        &mut words,
        fail_process_start_neg,
        fail_process_start,
        Reg::X0,
        63,
    );
    patch_cbz(
        &mut words,
        fail_process_start_zero,
        fail_process_start,
        Reg::X0,
    );
    patch_tbnz(
        &mut words,
        fail_process_wait_neg,
        fail_process_wait,
        Reg::X0,
        63,
    );
    patch_b_ne(
        &mut words,
        fail_process_wait_mask,
        fail_process_wait_bad_mask,
    );

    // Сериализуем в байты.
    let mut bytes = alloc::vec::Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.word().to_le_bytes());
    }
    bytes
}

fn userspace_self_spawn_via_syscalls() {
    let event = Event::new();
    let payload = build_parent_payload();
    // Запас под payload: количество movz/movk/store не помещается в одну
    // страницу.
    const PARENT_PAGES: usize = 4;
    let mut padded = alloc::vec::Vec::with_capacity(PARENT_PAGES * PAGE_SIZE);
    padded.extend_from_slice(&payload);
    while padded.len() < PARENT_PAGES * PAGE_SIZE {
        padded.push(0);
    }

    let segment = UserSegment {
        va_base: aligned(USER_PAYLOAD_VA),
        mapped_size: PARENT_PAGES * PAGE_SIZE,
        init_bytes: &padded,
        perms: MemFlags::user_rx(),
    };
    let image = UserImage {
        segments: core::slice::from_ref(&segment),
        entry: VirtualAddress::new(USER_PAYLOAD_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP),
        user_stack_size: USER_STACK_SIZE,
    };

    let handle = Handle::new(KObject::Event(event.clone()), Rights::SIGNAL | Rights::WAIT);
    let launch = UserProcessLaunch::new()
        .initial_handles(vec![handle])
        .bootstrap_handle(0);
    let info = kernelspace::qemu_tests::user_process_launcher()
        .spawn_user_process_with_launch("user-self-spawn", &image, Priority::highest(), 2, launch)
        .expect("spawn parent must succeed");
    test_harness_qemu::kassert_eq!(info.initial_handle_ids.len(), 1);
    let parent_thread = info.thread_object.clone();

    let scheduler = kernelspace::qemu_tests::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & (SUCCESS_SIGNAL | FAILURE_SIGNAL) == 0
        && parent_thread.peek() & THREAD_TERMINATED == 0
    {
        scheduler.sleep_ms(10);
        spins += 1;
        test_harness_qemu::kassert!(spins < 300, "self-spawn parent did not report status");
    }

    let observed = event.peek();
    if observed & FAILURE_SIGNAL != 0 {
        let mut exit_spins = 0u64;
        while parent_thread.peek() & THREAD_TERMINATED == 0 {
            scheduler.sleep_ms(10);
            exit_spins += 1;
            test_harness_qemu::kassert!(exit_spins < 50, "self-spawn failure path did not exit");
        }
        let code = parent_thread.exit_code();
        test_harness_qemu::kassert!(
            false,
            "userspace self-spawn payload failed at stage {} ({})",
            code,
            fail_stage_name(code)
        );
    }

    test_harness_qemu::kassert!(
        observed & SUCCESS_SIGNAL != 0,
        "self-spawn parent exited without success signal (signals={:#x}, terminated={})",
        observed,
        parent_thread.peek() & THREAD_TERMINATED != 0
    );

    let _ = USER_SEGMENT_SIZE;
}

fn fail_stage_name(code: i32) -> &'static str {
    match code {
        x if x == i32::from(FAIL_STAGE_REGION_CREATE) => "MemoryCreateVirtual",
        x if x == i32::from(FAIL_STAGE_REGION_MAP) => "MemoryMap",
        x if x == i32::from(FAIL_STAGE_REGION_REMAP) => "MemoryRemap",
        x if x == i32::from(FAIL_STAGE_DESC_ALLOC) => "MemoryAllocate(desc page)",
        x if x == i32::from(FAIL_STAGE_PROCESS_CREATE) => "ProcessCreate",
        x if x == i32::from(FAIL_STAGE_PROCESS_LOAD_IMAGE) => "ProcessLoadImage",
        x if x == i32::from(FAIL_STAGE_PROCESS_START) => "ProcessStart",
        x if x == i32::from(FAIL_STAGE_PROCESS_WAIT) => "ObjectWaitOne(child, PROCESS_TERMINATED)",
        x if x == i32::from(FAIL_STAGE_PROCESS_WAIT_MASK) => {
            "ObjectWaitOne returned unexpected mask"
        }
        _ => "unknown",
    }
}

register_test!(
    USERSPACE_SELF_SPAWN_VIA_SYSCALLS,
    "userspace_self_spawn_via_syscalls",
    userspace_self_spawn_via_syscalls
);
