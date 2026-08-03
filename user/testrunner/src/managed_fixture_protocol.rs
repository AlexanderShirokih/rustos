//! Typed startup-контракт managed-фикстуры.

#[ipc::protocol(name = "ManagedFixture")]
pub trait ManagedFixture {
    /// Передаёт authority, код завершения и режим удержания процесса.
    #[cast]
    fn start(&self, authority: ipc::wire::Cap, exit_code: u32, keep_running: bool);
}
