//! Объявление агрегата, передаваемого одним полем сообщения.

/// Объявляет struct и выводит для него [`WireValue`](crate::WireValue) и
/// [`WireTyped`](crate::WireTyped). Значение передаётся вложенным суб-кадром одной записи поля.
///
/// Раскрытие порождает структуру с именем, видимостью и полями из объявления, а следом -
/// оба impl. Поля идут в порядке объявления с позиционными `field_id` от единицы, каждое -
/// сам `WireValue`. Допускается не более одного лайфтайма, type- и const-генерики не
/// поддержаны.
///
/// Суб-кадр устроен как тело кадра: записи полей и терминатор. Капы полей агрегата
/// передаются в общих слотах кадра, и суб-кадр содержит их индексы. Отсутствие поля в
/// принятом суб-кадре - `MissingField`, неизвестное поле пропускается курсором. Число полей
/// агрегата вместе с вложенными проверяет const-ассерт на месте объявления.
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
            name = [$name $(<$lt>)?] lifetime = [$($lt)?]
            acc = [] id = $crate::wire::FIELD_ID_MIN;
            $($field : $ty),*
        );
    };

    // Позиционный `field_id` назначается обходом полей: раскрыться в список выражений макрос
    // не может, поэтому поля копятся в аккумуляторе.
    (@fields name = $name:tt lifetime = $lt:tt acc = [$($acc:tt)*] id = $id:expr;) => {
        $crate::wire_value!(@impl name = $name lifetime = $lt fields = [$($acc)*]);
    };

    (@fields name = $name:tt lifetime = $lt:tt acc = [$($acc:tt)*] id = $id:expr;
        $field:ident : $ty:ty $(, $($more:tt)*)?
    ) => {
        $crate::wire_value!(@fields
            name = $name lifetime = $lt
            acc = [$($acc)* { $id , $field , $ty }] id = $id + 1u8;
            $($($more)*)?
        );
    };

    (@impl name = [$name:ident] lifetime = [] fields = [$($fields:tt)*]) => {
        impl<'wire> $crate::WireValue<'wire> for $name {
            $crate::wire_value!(@body 'wire, $($fields)*);
        }

        impl $crate::WireTyped for $name {
            $crate::wire_value!(@desc $($fields)*);
        }

        $crate::wire_value!(@guard $name);
    };

    (@impl name = [$name:ident <$lt:lifetime>] lifetime = [$_lt:lifetime]
        fields = [$($fields:tt)*]
    ) => {
        impl<$lt> $crate::WireValue<$lt> for $name<$lt> {
            $crate::wire_value!(@body $lt, $($fields)*);
        }

        impl<$lt> $crate::WireTyped for $name<$lt> {
            $crate::wire_value!(@desc $($fields)*);
        }

        $crate::wire_value!(@guard $name);
    };

    (@guard $name:ident) => {
        const _: () = ::core::assert!(
            $crate::schema::type_fits_field_id_space(
                <$name as $crate::WireTyped>::WIRE_TYPE,
                $crate::wire::FIELD_ID_MAX,
            ),
            "aggregate declares more fields than the sub-frame field_id space carries"
        );
    };

    (@body $lt:lifetime, $( { $id:expr , $field:ident , $ty:ty } )*) => {
        fn write_as_field<const N: usize>(
            &self,
            buf: &mut $crate::wire::MessageBuf<N>,
            field_id: u8,
            handles: &mut $crate::wire::HandleSlots,
        ) -> ::core::result::Result<(), $crate::wire::IpcError> {
            let mut sub = $crate::wire::MessageBuf::<{ $crate::wire::FIELD_DATA_MAX }>::new();
            $(
                $crate::WireValue::write_as_field(&self.$field, &mut sub, $id, handles)?;
            )*
            sub.finish()?;
            buf.write_field(field_id, sub.as_bytes())
        }

        fn from_field(
            data: &$lt [u8],
            handles: &$crate::wire::HandleSlots,
        ) -> ::core::result::Result<Self, $crate::wire::IpcError> {
            $( let mut $field: ::core::option::Option<$ty> = ::core::option::Option::None; )*
            let mut cursor = $crate::wire::FieldCursor::new(data);
            while let ::core::option::Option::Some(entry) = cursor.next_field()? {
                $(
                    if entry.id == $id {
                        $field = ::core::option::Option::Some(
                            <$ty as $crate::WireValue>::from_field(entry.data, handles)?,
                        );
                    }
                )*
            }
            ::core::result::Result::Ok(Self {
                $(
                    $field: match $field {
                        ::core::option::Option::Some(value) => value,
                        ::core::option::Option::None => {
                            return ::core::result::Result::Err(
                                $crate::wire::IpcError::MissingField,
                            );
                        }
                    },
                )*
            })
        }
    };

    (@desc $( { $id:expr , $field:ident , $ty:ty } )*) => {
        const WIRE_TYPE: $crate::schema::WireType = $crate::schema::WireType::Aggregate(&[
            $( <$ty as $crate::WireTyped>::WIRE_TYPE ),*
        ]);
    };
}
