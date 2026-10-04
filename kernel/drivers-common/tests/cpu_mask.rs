use drivers_common::services::interrupts::CpuMask;

#[test]
fn cpu_mask_cpu_sets_single_bit() {
    let first = CpuMask::cpu(0);
    let last = CpuMask::cpu(7).map(CpuMask::raw);

    assert_eq!(first, Some(CpuMask::CPU0));
    assert_eq!(last, Some(0b1000_0000));
}

#[test]
fn cpu_mask_cpu_past_the_mask_width_is_none() {
    let past = CpuMask::cpu(8);
    let largest = CpuMask::cpu(u8::MAX);

    assert_eq!(past, None);
    assert_eq!(largest, None);
}
