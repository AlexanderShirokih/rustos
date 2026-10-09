//! Объявление агрегата, передаваемого одним полем сообщения.

/// Объявляет структуру, которая передаётся одним полем сообщения, и выводит для неё
/// [`WireValue`](crate::WireValue), [`IntoWireValue`](crate::IntoWireValue) и
/// [`WireTyped`](crate::WireTyped).
///
/// Поля кодируются в порядке объявления с `field_id` от единицы. Допускается не более одного
/// лайфтайма.
///
/// Тип поля-capability задаётся параметром владельца, умолчание которого - тип записи поля,
/// например `Cap<K>` либо [`Session<P>`](crate::wire::Session).
///
/// ```
/// ipc::wire_value! {
///     pub struct Extent<M = ipc::wire::Cap<syscall::kind::MemoryRegion>> {
///         pub region: M,
///         pub len: u32,
///     }
/// }
/// ```
///
/// Поле передаётся вложенной записью из записей полей и терминатора. Capability полей
/// передаются в общих слотах кадра. Поле, которого нет в принятом кадре, даёт `MissingField`,
/// неизвестное поле пропускается. Число полей вместе с вложенными проверяет const-ассерт.
#[macro_export]
macro_rules! wire_value {
    (
        $(#[$attr:meta])*
        $vis:vis struct $name:ident $(<$lt:lifetime>)? {
            $( $(#[$field_attr:meta])* $field_vis:vis $field:ident : $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$attr])*
        $vis struct $name $(<$lt>)? {
            $( $(#[$field_attr])* $field_vis $field: $ty, )*
        }

        $crate::wire_value!(@fields
            name = $name lifetime = [$($lt)?] owners = []
            acc = [] id = $crate::wire::FIELD_ID_MIN;
            $($field : $ty),*
        );
    };

    (
        $(#[$attr:meta])*
        $vis:vis struct $name:ident < $($lt:lifetime ,)? $($owner:ident = $default:ty),+ $(,)? > {
            $( $(#[$field_attr:meta])* $field_vis:vis $field:ident : $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$attr])*
        $vis struct $name < $($lt ,)? $($owner = $default),+ > {
            $( $(#[$field_attr])* $field_vis $field: $ty, )*
        }

        $crate::wire_value!(@fields
            name = $name lifetime = [$($lt)?] owners = [$($owner = $default),+]
            acc = [] id = $crate::wire::FIELD_ID_MIN;
            $($field : $ty),*
        );
    };

    // Позиционный `field_id` назначается обходом полей: раскрыться в список выражений макрос
    // не может, поэтому поля копятся в аккумуляторе.
    (@fields name = $name:ident lifetime = $lt:tt owners = $owners:tt
        acc = [$($acc:tt)*] id = $id:expr;
    ) => {
        $crate::wire_value!(@impl name = $name lifetime = $lt owners = $owners fields = [$($acc)*]);
    };

    (@fields name = $name:ident lifetime = $lt:tt owners = $owners:tt
        acc = [$($acc:tt)*] id = $id:expr;
        $field:ident : $ty:ty $(, $($more:tt)*)?
    ) => {
        $crate::wire_value!(@fields
            name = $name lifetime = $lt owners = $owners
            acc = [$($acc)* { $id , $field , $ty }] id = $id + 1u8;
            $($($more)*)?
        );
    };

    // Запись - умолчание. Псевдонимы параметров внутри блока подставляют умолчание в типы
    // полей, и прочитанное значение заменяет каждый параметр владельца формой, которую
    // получатель читает из его умолчания. `IntoWireValue` реализуется для любого владельца с
    // `Read` = умолчание, который пишется в кадр с ресурсами `__Frame`.
    (@impl name = $name:ident lifetime = [$($lt:lifetime)?] owners = [$($owner:ident = $default:ty),*]
        fields = [$($fields:tt)*]
    ) => {
        const _: () = {
            $( type $owner = $default; )*
            $crate::wire_value!(@read name = $name lifetime = [$($lt)?] owners = [$($owner),*]
                fields = [$($fields)*]);
        };

        impl<
            $($lt,)?
            __Frame: $crate::ReceivedResources,
            $($owner: $crate::IntoWireValue<__Frame, Read = $default>),*
        > $crate::IntoWireValue<__Frame> for $name<$($lt,)? $($owner),*>
        {
            type Read = $name $(<$lt>)?;

            $crate::wire_value!(@write name = $name, $($fields)*);
        }

        const _: () = ::core::assert!(
            $crate::schema::type_fits_field_id_space(
                <$name as $crate::WireTyped>::WIRE_TYPE,
                $crate::wire::FIELD_ID_MAX,
            ),
            "aggregate declares more fields than its nested record field_id space carries"
        );
    };

    (@read name = $name:ident lifetime = [] owners = [$($owner:ident),*] fields = [$($fields:tt)*]) => {
        impl<'wire, __Frame: $crate::ReceivedResources> $crate::WireValue<'wire, __Frame> for $name {
            type Owned = $name<$( <$owner as $crate::WireValue<'wire, __Frame>>::Owned ),*>;

            $crate::wire_value!(@from_field name = $name lifetime = 'wire, $($fields)*);
        }

        impl $crate::WireTyped for $name {
            $crate::wire_value!(@desc $($fields)*);
        }
    };

    (@read name = $name:ident lifetime = [$lt:lifetime] owners = [$($owner:ident),*]
        fields = [$($fields:tt)*]
    ) => {
        impl<$lt, __Frame: $crate::ReceivedResources> $crate::WireValue<$lt, __Frame> for $name<$lt> {
            type Owned = $name<$lt, $( <$owner as $crate::WireValue<$lt, __Frame>>::Owned ),*>;

            $crate::wire_value!(@from_field name = $name lifetime = $lt, $($fields)*);
        }

        impl<$lt> $crate::WireTyped for $name<$lt> {
            $crate::wire_value!(@desc $($fields)*);
        }
    };

    (@write name = $name:ident, $( { $id:expr , $field:ident , $ty:ty } )*) => {
        fn write_into(
            self,
            frame: &mut $crate::wire::MessageBuf<__Frame>,
            field_id: u8,
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            let $name { $($field),* } = self;
            frame.write_nested(field_id, |fields| {
                $( $crate::IntoWireValue::write_into($field, fields, $id)?; )*
                ::core::result::Result::Ok(())
            })
        }
    };

    (@from_field name = $name:ident lifetime = $lt:lifetime,
        $( { $id:expr , $field:ident , $ty:ty } )*
    ) => {
        fn from_field(
            data: &$lt [u8],
            frame: &mut __Frame,
        ) -> ::core::result::Result<Self::Owned, $crate::wire::IpcError> {
            $( let mut $field = ::core::option::Option::None; )*
            let mut cursor = $crate::wire::FieldCursor::new(data);
            while let ::core::option::Option::Some(entry) = cursor.next_field()? {
                $(
                    if entry.id == $id {
                        $field = ::core::option::Option::Some(entry.data);
                    }
                )*
            }
            $(
                let ::core::option::Option::Some($field) = $field else {
                    return ::core::result::Result::Err($crate::wire::IpcError::MissingField);
                };
            )*
            ::core::result::Result::Ok($name {
                $( $field: <$ty as $crate::WireValue<$lt, __Frame>>::from_field($field, frame)?, )*
            })
        }
    };

    (@desc $( { $id:expr , $field:ident , $ty:ty } )*) => {
        const WIRE_TYPE: $crate::schema::WireType = $crate::schema::WireType::Aggregate(&[
            $( <$ty as $crate::WireTyped>::WIRE_TYPE ),*
        ]);
    };
}
