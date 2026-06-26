use syscall::Rights;

use super::target::CapabilityTarget;

/// Стартовый набор прав для свежесозданного capability target данного типа.
pub fn default_rights_for(obj: &CapabilityTarget) -> Rights {
    match obj {
        // Полный набор «сигнальных» прав (READ|WRITE|DUPLICATE|TRANSFER):
        // - Signal сигналуем пользователем; Process/Thread терминацию поднимает
        //   только ядро (через bound-Signal), но WRITE оставлен под terminate-op;
        // - IrqControl: WRITE гейтит минт IrqLine, READ для инспекции,
        //   DUPLICATE/TRANSFER для делегирования вниз (как Resource);
        // - IrqLine: READ для ожидания (через as_waitable), WRITE гейтит ack,
        //   DUPLICATE/TRANSFER чтобы супервизор отдал линию драйверу.
        CapabilityTarget::Signal(_)
        | CapabilityTarget::Process(_)
        | CapabilityTarget::Thread(_)
        | CapabilityTarget::IrqControl(_)
        | CapabilityTarget::IrqLine(_) => {
            Rights::READ | Rights::WRITE | Rights::DUPLICATE | Rights::TRANSFER
        }

        CapabilityTarget::Memory(region) => {
            // База - только передаваемость/дублируемость; конкретный доступ
            // (READ/WRITE/EXECUTE) определяется access-маской региона, чтобы
            // read-only регион не получал WRITE по умолчанию.
            let mut rights = Rights::DUPLICATE | Rights::TRANSFER;
            let access = region.access_mask();
            if access.allows(memory::AccessMask::R) {
                rights |= Rights::READ;
            }
            if access.allows(memory::AccessMask::W) {
                rights |= Rights::WRITE;
            }
            if access.allows(memory::AccessMask::X) {
                rights |= Rights::EXECUTE;
            }
            rights
        }

        CapabilityTarget::Resource(_) => {
            Rights::DUPLICATE | Rights::TRANSFER | Rights::READ | Rights::WRITE
        }

        // Port: send гейтится WRITE, recv - READ (как channel
        // write/read); делегируется и дублируется.
        CapabilityTarget::Port(_) => {
            Rights::READ | Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE
        }

        // Reply: WRITE гейтит сам reply; TRANSFER даёт делегировать
        // ответ другому серверу. Не дублируется.
        CapabilityTarget::Reply(_) => Rights::WRITE | Rights::TRANSFER,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::{process::ProcessObject, target::CapabilityTarget, thread::ThreadObject},
        *,
    };

    #[test]
    fn defaults_for_process_grants_write_and_read() {
        let target = CapabilityTarget::Process(ProcessObject::new());
        let r = default_rights_for(&target);
        assert!(r.contains(Rights::WRITE));
        assert!(r.contains(Rights::READ));
        assert!(r.contains(Rights::DUPLICATE));
        assert!(r.contains(Rights::TRANSFER));
    }

    #[test]
    fn defaults_for_process_omits_execute() {
        let target = CapabilityTarget::Process(ProcessObject::new());
        let r = default_rights_for(&target);
        assert!(!r.contains(Rights::EXECUTE));
    }

    #[test]
    fn defaults_for_thread_grants_write_and_read() {
        let target = CapabilityTarget::Thread(ThreadObject::new());
        let r = default_rights_for(&target);
        assert!(r.contains(Rights::WRITE));
        assert!(r.contains(Rights::READ));
        assert!(r.contains(Rights::DUPLICATE));
        assert!(r.contains(Rights::TRANSFER));
    }

    #[test]
    fn defaults_for_thread_omits_execute() {
        let target = CapabilityTarget::Thread(ThreadObject::new());
        let r = default_rights_for(&target);
        assert!(!r.contains(Rights::EXECUTE));
    }

    fn memory_region(access: memory::AccessMask) -> CapabilityTarget {
        use core::{
            num::NonZeroUsize,
            sync::atomic::{AtomicUsize, Ordering},
        };

        use memory::{
            MemoryRegion,
            frame::Frame,
            frame_allocator::{FrameAllocator, FrameError, ReserveFrameError},
        };

        struct StubFrameAllocator {
            next: AtomicUsize,
        }

        impl FrameAllocator for StubFrameAllocator {
            fn reserve_frames_exact(
                &self,
                from_inclusive: Frame,
                _to_exclusive: Frame,
            ) -> Result<Frame, ReserveFrameError> {
                Ok(from_inclusive)
            }
            fn allocate_frame(&self) -> Option<Frame> {
                Some(Frame::new(self.next.fetch_add(1, Ordering::Relaxed)))
            }
            fn allocate_frames(&self, _max_count: usize) -> Option<(Frame, usize)> {
                None
            }
            fn deallocate_frame(&self, _frame: Frame) -> Result<(), FrameError> {
                Ok(())
            }
            fn is_allocated(&self, _frame: Frame) -> bool {
                false
            }
        }

        let fa: &'static (dyn FrameAllocator + Send + Sync) =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(StubFrameAllocator {
                next: AtomicUsize::new(0x1000),
            }));
        let region = MemoryRegion::create_virtual(fa, NonZeroUsize::new(1).unwrap(), access)
            .expect("stub frame allocator never runs out");
        CapabilityTarget::Memory(alloc::sync::Arc::new(region))
    }

    #[test]
    fn defaults_for_memory_mirrors_access_mask() {
        // RW-регион: READ+WRITE, без EXECUTE; всегда DUPLICATE+TRANSFER.
        let r = default_rights_for(&memory_region(memory::AccessMask::RW));
        assert!(r.contains(Rights::READ));
        assert!(r.contains(Rights::WRITE));
        assert!(!r.contains(Rights::EXECUTE));
        assert!(r.contains(Rights::DUPLICATE));
        assert!(r.contains(Rights::TRANSFER));
    }

    #[test]
    fn defaults_for_memory_readonly_omits_write() {
        // RO-регион не должен получать WRITE по умолчанию.
        let r = default_rights_for(&memory_region(memory::AccessMask::R));
        assert!(r.contains(Rights::READ));
        assert!(!r.contains(Rights::WRITE));
        assert!(!r.contains(Rights::EXECUTE));
    }

    #[test]
    fn defaults_for_memory_executable_grants_execute() {
        // RX-регион: READ+EXECUTE, без WRITE.
        let r = default_rights_for(&memory_region(memory::AccessMask::RX));
        assert!(r.contains(Rights::READ));
        assert!(!r.contains(Rights::WRITE));
        assert!(r.contains(Rights::EXECUTE));
    }

    #[test]
    fn defaults_for_port_grants_read_write_transfer_duplicate() {
        use super::super::port::Port;
        let r = default_rights_for(&CapabilityTarget::Port(Port::new()));
        assert!(r.contains(Rights::READ));
        assert!(r.contains(Rights::WRITE));
        assert!(r.contains(Rights::TRANSFER));
        assert!(r.contains(Rights::DUPLICATE));
        assert!(!r.contains(Rights::EXECUTE));
    }

    #[test]
    fn defaults_for_resource_grants_rw_transfer_duplicate_no_execute() {
        use core::num::NonZeroUsize;

        use memory::physical_address::PageAlignedAddress;

        use super::super::resource::Resource;
        let resource = Resource::new(
            PageAlignedAddress::from_usize(0x4000_0000).unwrap(),
            NonZeroUsize::new(0x1000).unwrap(),
            memory::AccessMask::RW,
            0,
        );
        let r = default_rights_for(&CapabilityTarget::Resource(resource));
        assert!(r.contains(Rights::READ));
        assert!(r.contains(Rights::WRITE));
        assert!(r.contains(Rights::DUPLICATE));
        assert!(r.contains(Rights::TRANSFER));
        assert!(!r.contains(Rights::EXECUTE));
    }
}
