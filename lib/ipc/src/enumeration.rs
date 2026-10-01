//! Объявление перечисления, передаваемого одним полем сообщения.

/// Объявляет перечисление без данных в вариантах, которое передаётся одним полем сообщения, и
/// выводит для него [`WireValue`](crate::WireValue), [`IntoWireValue`](crate::IntoWireValue),
/// [`WireTyped`](crate::WireTyped), `From<Enum>` для целого дискриминанта и `TryFrom` из него.
///
/// Вариант передаётся дискриминантом из объявления в порядке little-endian. Тип дискриминанта
/// указывается после имени перечисления, без него дискриминант - `u16`. Дискриминант вне
/// объявленных даёт `InvalidValue`.
///
/// ```
/// ipc::wire_enum! {
///     #[derive(Debug, Clone, Copy, PartialEq, Eq)]
///     pub enum Refusal {
///         Busy = 1,
///         Denied = 2,
///     }
/// }
///
/// ipc::wire_enum! {
///     #[derive(Debug, Clone, Copy, PartialEq, Eq)]
///     pub enum Side: u8 {
///         Left = 1,
///         Right = 2,
///     }
/// }
/// ```
#[macro_export]
macro_rules! wire_enum {
    (
        $(#[$attr:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$variant_attr:meta])* $variant:ident = $value:literal ),+ $(,)?
        }
    ) => {
        $crate::wire_enum! {
            $(#[$attr])*
            $vis enum $name: u16 {
                $( $(#[$variant_attr])* $variant = $value, )+
            }
        }
    };
    (
        $(#[$attr:meta])*
        $vis:vis enum $name:ident: $repr:ident {
            $( $(#[$variant_attr:meta])* $variant:ident = $value:literal ),+ $(,)?
        }
    ) => {
        $(#[$attr])*
        #[repr($repr)]
        $vis enum $name {
            $( $(#[$variant_attr])* $variant = $value, )+
        }

        impl ::core::convert::From<$name> for $repr {
            fn from(value: $name) -> Self {
                value as $repr
            }
        }

        impl ::core::convert::TryFrom<$repr> for $name {
            type Error = $crate::wire::IpcError;

            fn try_from(value: $repr) -> ::core::result::Result<Self, Self::Error> {
                match value {
                    $( $value => ::core::result::Result::Ok(Self::$variant), )+
                    _ => ::core::result::Result::Err($crate::wire::IpcError::InvalidValue),
                }
            }
        }

        impl<'a, R: $crate::ReceivedResources> $crate::WireValue<'a, R> for $name {
            type Owned = Self;

            fn from_field(
                data: &'a [u8],
                _frame: &mut R,
            ) -> ::core::result::Result<Self, $crate::wire::IpcError> {
                let bytes = data
                    .try_into()
                    .map_err(|_| $crate::wire::IpcError::InvalidValue)?;

                Self::try_from($repr::from_le_bytes(bytes))
            }
        }

        impl<R: $crate::ReceivedResources> $crate::IntoWireValue<R> for $name {
            type Read = Self;

            fn write_into(
                self,
                frame: &mut $crate::wire::MessageBuf<R>,
                field_id: u8,
            ) -> ::core::result::Result<(), $crate::wire::IpcError> {
                frame.write_field(field_id, &$repr::from(self).to_le_bytes())
            }
        }

        impl $crate::WireTyped for $name {
            #[allow(clippy::cast_possible_truncation)]
            const WIRE_TYPE: $crate::schema::WireType =
                $crate::schema::WireType::Uint(::core::mem::size_of::<$repr>() as u8);
        }
    };
}
