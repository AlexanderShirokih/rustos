//! Структурный дескриптор записи поля.
//!
//! `protocol!` и `wire_value!` собирают дескрипторы полей в const-контексте и проверяют по ним
//! бюджет кадра и пространство `field_id`.

/// Размер заголовка кадра в байтах: `ordinal`.
pub const HEADER_SIZE: usize = 8;

/// Накладные расходы одной записи поля: `field_id` (1 байт) + длина (`u16` LE).
pub const FIELD_OVERHEAD: usize = 3;

/// Тег типа значения в сообщении.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    /// Беззнаковое целое заданной ширины в байтах.
    Uint(u8),
    /// Знаковое целое заданной ширины в байтах.
    Int(u8),
    /// Логическое значение (один байт).
    Bool,
    /// UTF-8 строка, длину которой ограничивает кадр.
    Str,
    /// Байты, длину которых ограничивает кадр.
    Bytes,
    /// UTF-8 строка владеющего значения с ёмкостью `N` байт.
    BoundedStr(usize),
    /// Байты владеющего значения с ёмкостью `N`.
    BoundedBytes(usize),
    /// Capability: handle передаётся вне тела, в поле - индекс в handle-массиве.
    Capability,
    /// Агрегат: поля в порядке объявления, каждое - вложенный `WireType`.
    Aggregate(&'static [WireType]),
    /// Исход `Result`: вложенная запись содержит ровно одно поле - успех либо доменную ошибку.
    Result {
        /// Тип успешного исхода.
        ok: &'static WireType,
        /// Тип доменной ошибки.
        err: &'static WireType,
    },
    /// Необязательное значение. Поле с пустыми данными означает его отсутствие.
    Optional(&'static WireType),
}

/// Бывает ли запись поля такого типа пустой никогда.
#[must_use]
pub const fn is_never_empty(field_type: WireType) -> bool {
    match field_type {
        WireType::Uint(_)
        | WireType::Int(_)
        | WireType::Bool
        | WireType::Capability
        | WireType::Aggregate(_)
        | WireType::Result { .. } => true,
        WireType::Str
        | WireType::Bytes
        | WireType::BoundedStr(_)
        | WireType::BoundedBytes(_)
        | WireType::Optional(_) => false,
    }
}

/// Наибольшая длина данных одной записи поля такого типа. Длину данных строки и байтов без
/// ёмкости считает нулевой.
#[must_use]
pub const fn max_data_len(field_type: WireType) -> usize {
    match field_type {
        WireType::Uint(width) | WireType::Int(width) => width as usize,
        WireType::Str | WireType::Bytes => 0,
        WireType::Bool | WireType::Capability => 1,
        WireType::BoundedStr(bound) | WireType::BoundedBytes(bound) => bound,
        // Агрегат передаётся вложенной записью, и её поля добавляют свои накладные расходы.
        WireType::Aggregate(fields) => max_body_len(fields),
        WireType::Result { ok, err } => {
            let ok = max_data_len(*ok);
            let err = max_data_len(*err);
            1 + FIELD_OVERHEAD + if ok > err { ok } else { err }
        }
        WireType::Optional(value) => max_data_len(*value),
    }
}

/// Наибольшая длина тела из записей полей таких типов, включая терминатор. Длину данных строки
/// и байтов без ёмкости считает нулевой.
#[must_use]
pub const fn max_body_len(fields: &[WireType]) -> usize {
    let mut total = 1;
    let mut pending = fields;
    while let [field, tail @ ..] = pending {
        total += FIELD_OVERHEAD + max_data_len(*field);
        pending = tail;
    }
    total
}

/// Умещаются ли позиционные `field_id` агрегата, исхода `Result` и их вложенных записей в
/// `field_id_max`.
#[must_use]
pub const fn type_fits_field_id_space(field_type: WireType, field_id_max: u8) -> bool {
    match field_type {
        WireType::Aggregate(fields) => {
            if fields.len() > field_id_max as usize {
                return false;
            }
            let mut pending = fields;
            while let [field, tail @ ..] = pending {
                if !type_fits_field_id_space(*field, field_id_max) {
                    return false;
                }
                pending = tail;
            }
            true
        }
        // Успех и ошибка занимают поля 1 и 2 вложенной записи.
        WireType::Result { ok, err } => {
            field_id_max >= 2
                && type_fits_field_id_space(*ok, field_id_max)
                && type_fits_field_id_space(*err, field_id_max)
        }
        WireType::Optional(value) => type_fits_field_id_space(*value, field_id_max),
        _ => true,
    }
}

/// Содержит ли запись типа `field_type` capability, в том числе во вложенных записях.
#[must_use]
pub const fn carries_capability(field_type: WireType) -> bool {
    match field_type {
        WireType::Capability => true,
        WireType::Aggregate(fields) => {
            let mut pending = fields;
            while let [field, tail @ ..] = pending {
                if carries_capability(*field) {
                    return true;
                }
                pending = tail;
            }
            false
        }
        WireType::Result { ok, err } => carries_capability(*ok) || carries_capability(*err),
        WireType::Optional(value) => carries_capability(*value),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WireType, carries_capability, is_never_empty, max_body_len, max_data_len,
        type_fits_field_id_space,
    };

    const FIELD_ID_MAX: u8 = 254;

    #[test]
    fn aggregate_data_length_counts_its_own_records() {
        // Терминатор вложенной записи плюс две записи по три байта накладных расходов.
        assert_eq!(
            max_data_len(WireType::Aggregate(&[WireType::Uint(4), WireType::Bool])),
            1 + (3 + 4) + (3 + 1)
        );
    }

    #[test]
    fn body_length_counts_every_field_and_the_terminator() {
        assert_eq!(
            max_body_len(&[WireType::BoundedStr(16), WireType::BoundedBytes(16)]),
            1 + 2 * (3 + 16)
        );
        assert_eq!(max_body_len(&[WireType::Str, WireType::Bytes]), 1 + 2 * 3);
        assert_eq!(max_body_len(&[]), 1);
    }

    #[test]
    fn result_data_length_takes_the_wider_outcome() {
        assert_eq!(
            max_data_len(WireType::Result {
                ok: &WireType::Uint(8),
                err: &WireType::Uint(2),
            }),
            1 + 3 + 8
        );
    }

    #[test]
    fn fields_beyond_the_frame_space_are_rejected() {
        const WIDE: [WireType; 3] = [WireType::Bool, WireType::Bool, WireType::Bool];

        assert!(type_fits_field_id_space(WireType::Aggregate(&WIDE), 3));
        assert!(!type_fits_field_id_space(WireType::Aggregate(&WIDE), 2));
        assert!(type_fits_field_id_space(
            WireType::Aggregate(&WIDE),
            FIELD_ID_MAX
        ));
    }

    #[test]
    fn nested_aggregate_wider_than_the_frame_space_is_rejected() {
        const WIDE: [WireType; 3] = [WireType::Bool, WireType::Bool, WireType::Bool];
        const NESTED: [WireType; 1] = [WireType::Aggregate(&WIDE)];

        // Агрегат нумерует поля так же позиционно, и вложенный считается наравне.
        assert!(type_fits_field_id_space(WireType::Aggregate(&NESTED), 3));
        assert!(!type_fits_field_id_space(WireType::Aggregate(&NESTED), 2));
    }

    #[test]
    fn only_strings_bytes_unit_and_options_may_be_empty() {
        for (field_type, never_empty) in [
            (WireType::Uint(4), true),
            (WireType::Int(2), true),
            (WireType::Bool, true),
            (WireType::Capability, true),
            (WireType::Aggregate(&[]), true),
            (
                WireType::Result {
                    ok: &WireType::BoundedBytes(0),
                    err: &WireType::Uint(1),
                },
                true,
            ),
            (WireType::Str, false),
            (WireType::Bytes, false),
            (WireType::BoundedStr(16), false),
            (WireType::BoundedBytes(16), false),
            (WireType::BoundedBytes(0), false),
            (WireType::Optional(&WireType::Uint(4)), false),
        ] {
            assert_eq!(is_never_empty(field_type), never_empty, "{field_type:?}");
        }
    }

    #[test]
    fn an_optional_aggregate_counts_toward_field_id_space() {
        const WIDE: WireType =
            WireType::Aggregate(&[WireType::Bool, WireType::Bool, WireType::Bool]);

        assert!(type_fits_field_id_space(WireType::Optional(&WIDE), 3));
        assert!(!type_fits_field_id_space(WireType::Optional(&WIDE), 2));
    }

    #[test]
    fn an_optional_capability_carries_a_capability() {
        assert!(carries_capability(WireType::Optional(
            &WireType::Capability
        )));
        assert!(!carries_capability(WireType::Optional(&WireType::Uint(4))));
    }
}
