//! Исполнение команды старта вторичного CPU.
//!
//! Разбор device tree лежит в `hal_common::smp`; здесь - сам вызов PSCI и
//! освобождение CPU, ждущего на spin-table.

use hal_common::{
    cpu::CpuAffinity,
    smp::{PsciConduit, PsciFailure, StartMethod},
};

use crate::{
    consts::HIGHER_HALF_BASE,
    memory::regs::{common::EL1, dcache::DataCache, translate::AddressTranslation},
};

/// Ошибка старта CPU.
#[derive(Debug, PartialEq, Eq)]
pub enum StartError {
    /// Device tree не описывает способ старта.
    MethodUnknown,
    /// `CPU_ON` вернул код ошибки.
    PsciRejected(PsciFailure),
    /// `CPU_ON` вернул значение вне диапазона кодов PSCI.
    PsciMalformed(u64),
    /// Адрес из `cpu-release-addr` не отображён на запись.
    ReleaseAddressUnmapped(usize),
    /// CPU не доложил о готовности за отведённое время.
    NotReady,
}

impl StartError {
    /// Слот параметров остался за целевым CPU: его состояние неизвестно, и
    /// публиковать в слоте следующий идентификатор нельзя.
    pub fn leaves_slot_claimed(&self) -> bool {
        match self {
            // Команда старта не отдавалась: слот свободен.
            StartError::MethodUnknown | StartError::ReleaseAddressUnmapped(_) => false,
            // Разбор ответа PSCI: часть кодов оставляет CPU нетронутым.
            StartError::PsciRejected(status) => status.may_have_started(),
            // Ответ не разобран или CPU молчит: судить об итоге не по чему.
            StartError::PsciMalformed(_) | StartError::NotReady => true,
        }
    }
}

impl core::fmt::Display for StartError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StartError::MethodUnknown => {
                f.write_str("device tree declares no way to start the CPU")
            }
            StartError::PsciRejected(status) => {
                write!(f, "PSCI CPU_ON rejected the call: {status}")
            }
            StartError::PsciMalformed(value) => {
                write!(
                    f,
                    "PSCI CPU_ON returned a value outside the code range: {value:#x}"
                )
            }
            StartError::ReleaseAddressUnmapped(address) => {
                write!(
                    f,
                    "the release address {address:#x} is not mapped for writing"
                )
            }
            StartError::NotReady => f.write_str("the CPU did not report readiness in time"),
        }
    }
}

/// Запускает CPU с заданным affinity, передавая ему точку входа.
///
/// # Safety
///
/// `entry_point` - физический адрес исполняемого кода, а сам CPU не исполняет
/// код ядра в момент вызова.
pub unsafe fn start(
    method: StartMethod,
    affinity: CpuAffinity,
    entry_point: usize,
) -> Result<(), StartError> {
    match method {
        StartMethod::Psci { conduit, cpu_on } => {
            // SAFETY: контракт функции; `CPU_ON` принимает affinity цели,
            // физический адрес точки входа и context_id.
            let result = unsafe {
                psci_call(
                    conduit,
                    u64::from(cpu_on),
                    affinity.raw(),
                    entry_point as u64,
                    0,
                )
            };

            match i32::try_from(result.cast_signed()) {
                Ok(code) => PsciFailure::from_code(code).map_err(StartError::PsciRejected),
                Err(_) => Err(StartError::PsciMalformed(result)),
            }
        }
        StartMethod::SpinTable { release_address } => {
            // SAFETY: контракт функции; адрес освобождения объявлен узлом CPU,
            // а его отображение проверяется внутри.
            unsafe { release_spin_table(release_address, entry_point) }
        }
    }
}

/// # Safety
///
/// Вызов уходит на уровень выше EL1 и обязан быть корректным вызовом PSCI.
unsafe fn psci_call(conduit: PsciConduit, function: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    let result: u64;

    match conduit {
        PsciConduit::Hvc => {
            // SAFETY: `hvc #0` - вызов гипервизора по SMCCC: аргументы в x0-x3,
            // результат в x0. Регистры x4-x17 соглашение объявляет затираемыми.
            unsafe {
                core::arch::asm!(
                    "hvc #0",
                    inlateout("x0") function => result,
                    inlateout("x1") arg0 => _,
                    inlateout("x2") arg1 => _,
                    inlateout("x3") arg2 => _,
                    lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
                    lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _,
                    lateout("x12") _, lateout("x13") _, lateout("x14") _, lateout("x15") _,
                    lateout("x16") _, lateout("x17") _,
                    options(nostack),
                );
            }
        }
        PsciConduit::Smc => {
            // SAFETY: см. ветку `hvc`; `smc #0` отличается только уровнем,
            // на который уходит вызов.
            unsafe {
                core::arch::asm!(
                    "smc #0",
                    inlateout("x0") function => result,
                    inlateout("x1") arg0 => _,
                    inlateout("x2") arg1 => _,
                    inlateout("x3") arg2 => _,
                    lateout("x4") _, lateout("x5") _, lateout("x6") _, lateout("x7") _,
                    lateout("x8") _, lateout("x9") _, lateout("x10") _, lateout("x11") _,
                    lateout("x12") _, lateout("x13") _, lateout("x14") _, lateout("x15") _,
                    lateout("x16") _, lateout("x17") _,
                    options(nostack),
                );
            }
        }
    }

    result
}

/// # Safety
///
/// `release_address` - физический адрес, объявленный узлом CPU.
unsafe fn release_spin_table(release_address: usize, entry_point: usize) -> Result<(), StartError> {
    // Адрес приходит из device tree, а линейное отображение покрывает только
    // память, описанную им же. Проверка исключает запись по неотображённому
    // адресу; принадлежность адреса именно spin-table она не устанавливает.
    let Some(slot) = release_address.checked_add(HIGHER_HALF_BASE) else {
        return Err(StartError::ReleaseAddressUnmapped(release_address));
    };
    if !AddressTranslation::<EL1>::is_writable(slot) {
        return Err(StartError::ReleaseAddressUnmapped(release_address));
    }

    let slot = slot as *mut u64;

    // SAFETY: проверка выше подтвердила, что адрес транслируется на запись.
    // Ждущий CPU читает слот мимо кэша, и строка вытесняется следом.
    unsafe { slot.write_volatile(entry_point as u64) };

    DataCache::<EL1>::clean_range_to_coherency(slot as usize, slot as usize + size_of::<u64>());

    // SAFETY: `sev` будит CPU, ждущие на `wfe`, и не имеет других эффектов.
    unsafe { core::arch::asm!("sev", options(nomem, nostack, preserves_flags)) };

    Ok(())
}
