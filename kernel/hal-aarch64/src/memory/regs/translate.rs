//! Запрос трансляции адреса у MMU: инструкция `AT` и её результат в `PAR_EL1`.

use core::marker::PhantomData;

use super::common::{EL1, ExceptionLevel};

const PAR_FAULT: u64 = 1;

/// Запрос трансляции для уровня исключения `E`.
pub struct AddressTranslation<EL: ExceptionLevel> {
    _level: PhantomData<EL>,
}

impl AddressTranslation<EL1> {
    /// `true`, если `address` транслируется на EL1 с правом записи.
    ///
    /// `AT S1E1W` прогоняет адрес через тот же stage 1, что и обычный доступ,
    /// но к памяти не обращается и исключения на неотображённом адресе не
    /// берёт: отказ виден в `PAR_EL1.F`. Это единственный способ спросить
    /// "этот адрес отображён?", не рискуя самим обращением.
    pub fn is_writable(address: usize) -> bool {
        let par: u64;

        // SAFETY: `at` не читает и не пишет память, а лишь заполняет `PAR_EL1`
        // текущего CPU; `isb` требуется до чтения результата. Регистр читается
        // тем же блоком, поэтому между запросом и чтением ничего не вклинится.
        unsafe {
            core::arch::asm!(
                "at     s1e1w, {address}",
                "isb",
                "mrs    {par}, par_el1",
                address = in(reg) address,
                par = out(reg) par,
                options(nostack, preserves_flags),
            );
        }

        par & PAR_FAULT == 0
    }
}
