//! Держит инкарнацию компонента композиции: её процесс, резерв бюджета и одноразовый
//! стартовый кадр.
//!
//! [`ManagedComponent`] владеет одной инкарнацией. Метод `launch` поднимает процесс по
//! программе `EntryView` и отправляет ему стартовый кадр, `stop` завершает процесс,
//! `observe_exit` забирает код выхода. Состояние инкарнации названо [`ManagedStatus`], отказ -
//! [`ManagedFailure`].
//!
//! [`ComponentRoots`] держит control- и service-порты компонента на стороне supervisor.
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
