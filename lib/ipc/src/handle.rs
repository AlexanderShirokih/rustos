/// Источник capability для исходящего кадра даёт wire-id хэндла и по итогу
/// передачи решает судьбу владения.
///
/// `relinquish` зовётся только при успешной передаче - хэндл забрало ядро, и
/// реализатор обязан больше его не закрывать. При неуспехе `relinquish` не
/// зовётся. Значение роняется, и любая очистка идёт через его `Drop`.
pub trait IntoWireHandle {
    /// Вид отправляемого объекта. Он связывает источник с объявленным видом поля.
    type Kind: syscall::ObjectKind;

    /// Wire-id хэндла для записи в исходящий handle-вектор. Не потребляет.
    fn wire_id(&self) -> u32;

    /// Отдаёт владение после успешной передачи. Ядро забрало хэндл, закрытие
    /// подавляется.
    fn relinquish(self);

    /// Кладёт хэндл в слоты кадра, а в тело - запись поля с его индексом.
    fn write_as_field<const N: usize>(
        &self,
        buf: &mut crate::wire::MessageBuf<N>,
        field_id: u8,
        handles: &mut crate::wire::HandleSlots,
    ) -> Result<(), crate::wire::IpcError> {
        let index = handles.push(self.wire_id())?;
        buf.write_field(field_id, &crate::wire::value::encode_port_index(index))
    }
}

/// Ответ two-way операции, отдающий владение хэндлом. Сервис объявляет возврат
/// `impl IntoWireHandle`, а `dispatch` после успешной передачи зовёт `relinquish`.
pub trait WireReplyHandle {
    /// Флаги заголовка поверх `FLAG_RESPONSE`. Ответ с хэндлом доменной ошибки не
    /// передаёт, и `FLAG_DOMAIN_ERR` в таком ответе не стоит.
    fn reply_flags(&self) -> u16 {
        0
    }

    /// Кодирует хэндл в единственную запись тела ответа.
    fn write_reply<const N: usize>(
        &self,
        buf: &mut crate::wire::MessageBuf<N>,
        handles: &mut crate::wire::HandleSlots,
    ) -> Result<(), crate::wire::IpcError>;
}

impl<H: IntoWireHandle> WireReplyHandle for H {
    fn write_reply<const N: usize>(
        &self,
        buf: &mut crate::wire::MessageBuf<N>,
        handles: &mut crate::wire::HandleSlots,
    ) -> Result<(), crate::wire::IpcError> {
        self.write_as_field(buf, crate::wire::FIELD_ID_MIN, handles)
    }
}

impl<K: syscall::ObjectKind> IntoWireHandle for crate::wire::Cap<K> {
    type Kind = K;

    fn wire_id(&self) -> u32 {
        self.raw().get()
    }

    fn relinquish(self) {
        // Cap не владеет handle, отдавать нечего.
    }
}
