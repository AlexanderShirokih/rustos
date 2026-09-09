//! Держит инкарнацию компонента композиции: её процесс, резерв бюджета и startup-порт.
//!
//! [`ManagedComponent`] владеет одной инкарнацией. Метод `launch` поднимает процесс по
//! программе `EntryView` и отправляет ему стартовый кадр, `stop` завершает процесс,
//! `observe_exit` забирает код выхода. Состояние инкарнации названо [`ManagedStatus`], отказ -
//! [`ManagedFailure`].
//!
//! [`ComponentRoots`] держит порты экспортов компонента на стороне supervisor, а
//! [`RunningComponent`] - клиентскую сторону его startup-порта.
//! [`ComponentSlots`] копит capability слотов до отправки. [`launch_component`] собирает эти
//! части в один подъём.
//!
//! Уход [`ManagedComponent`] из области видимости завершает живой процесс.

#![no_std]

mod component;
mod roots;

pub use component::{
    BoundedStartupTransport, ManagedComponent, ManagedFailure, ManagedStartupError, ManagedStatus,
    RunningComponent,
};
pub use roots::{ComponentRoots, ComponentSlots, launch_component};
