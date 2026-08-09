//! Typed startup-контракт managed-фикстуры.

ipc::protocol! {
    /// Контракт управляемой фикстуры.
    pub mod managed_fixture {
        name = "ManagedFixture";

        /// Передаёт authority, код завершения и режим удержания процесса.
        cast fn start(authority: Cap, exit_code: u32, keep_running: bool);
    }
}
