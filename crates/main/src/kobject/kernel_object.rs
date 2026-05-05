use super::{koid::Koid, object_type::ObjectType};

/// Kernel-объект (KO) - единица, к которой ядро выдаёт права.
///
/// Любой ресурс или примитив, доступ к которому процесс получает не напрямую, а через capability -
/// это KO: канал, событие, таймер, в перспективе поток, MMIO-регион, IRQ. Один KO может быть
/// доступен нескольким процессам через разные handle'ы с разными правами.
pub trait KernelObject: Send + Sync {
    fn koid(&self) -> Koid;
    fn object_type(&self) -> ObjectType;
}
