//! Дескриптор схемы протокола.
//!
//! Машинно-читаемое описание контракта (операции, поля, ordinal),
//! достаточное для генерации совместимого клиента без чтения исходника.
//! `protocol!` собирает дескриптор в const-контексте и проверяет по нему сам контракт:
//! бюджет кадра, пространство `field_id` и совместимость с транспортной плоскостью.

/// Размер заголовка кадра в байтах (`ordinal` + `txid` + `flags`).
pub const HEADER_SIZE: usize = 14;

/// Накладные расходы одной записи поля: `field_id` (1 байт) + длина (`u16` LE).
pub const FIELD_OVERHEAD: usize = 3;

/// Вид операции протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Запрос-ответ клиент->сервер.
    Call,
    /// Односторонняя операция клиент->сервер без ответа.
    Cast,
    /// Односторонняя операция сервер->клиент без ответа.
    Event,
}

/// Тег типа значения в сообщении.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    /// Беззнаковое целое заданной ширины в байтах.
    Uint(u8),
    /// Знаковое целое заданной ширины в байтах.
    Int(u8),
    /// Логическое значение (один байт).
    Bool,
    /// Bounded UTF-8 строка с границей длины `N`.
    BoundedStr(usize),
    /// Bounded байтовая строка с границей длины `N`.
    BoundedBytes(usize),
    /// Capability: handle едет вне тела, в поле - индекс в handle-массиве.
    Capability,
    /// Агрегат: поля в порядке объявления, каждое - вложенный `WireType`.
    Aggregate(&'static [WireType]),
}

/// Описание одного поля операции: позиционный `field_id` и тип значения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDesc {
    /// Позиционный идентификатор поля (`1..=254`).
    pub field_id: u8,
    /// Тип значения поля в сообщении.
    pub field_type: WireType,
}

/// Описание одной операции протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationDesc {
    /// Имя операции (без префикса протокола).
    pub name: &'static str,
    /// Каноническое имя операции (`<protocol>.<operation>`).
    pub canonical: &'static str,
    /// Идентификатор операции (хэш канонического имени).
    pub ordinal: u64,
    /// Вид операции.
    pub kind: Kind,
    /// Поля параметров операции в порядке объявления.
    pub fields: &'static [FieldDesc],
}

/// Дескриптор протокола: список операций.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolDesc {
    /// Операции протокола в порядке объявления.
    pub operations: &'static [OperationDesc],
}

/// Наибольшая длина данных одной записи поля такого типа.
#[must_use]
pub const fn max_data_len(field_type: WireType) -> usize {
    match field_type {
        WireType::Uint(width) | WireType::Int(width) => width as usize,
        WireType::Bool | WireType::Capability => 1,
        WireType::BoundedStr(bound) | WireType::BoundedBytes(bound) => bound,
        // Агрегат едет вложенным суб-кадром: его поля со своими накладными расходами.
        WireType::Aggregate(fields) => max_body_len(fields),
    }
}

const fn max_body_len(fields: &[WireType]) -> usize {
    let mut total = 1;
    let mut pending = fields;
    while let [field, tail @ ..] = pending {
        total += FIELD_OVERHEAD + max_data_len(*field);
        pending = tail;
    }
    total
}

/// Укладывается ли кадр самой объёмной операции в `body_max` байт тела.
///
/// Границы bounded-типов известны в const-контексте: предел контракта считается на месте
/// объявления, до первой передачи.
#[must_use]
pub const fn fits_body_budget(desc: &ProtocolDesc, body_max: usize) -> bool {
    let mut operations = desc.operations;
    while let [operation, tail @ ..] = operations {
        let mut total = 1;
        let mut fields = operation.fields;
        while let [field, others @ ..] = fields {
            total += FIELD_OVERHEAD + max_data_len(field.field_type);
            fields = others;
        }
        if total > body_max {
            return false;
        }
        operations = tail;
    }
    true
}

/// Обходится ли контракт односторонними операциями, которые несёт SPSC-кольцо.
#[must_use]
pub const fn is_one_way(desc: &ProtocolDesc) -> bool {
    let mut operations = desc.operations;
    while let [operation, tail @ ..] = operations {
        if matches!(operation.kind, Kind::Call) {
            return false;
        }
        operations = tail;
    }
    true
}

/// Умещаются ли позиционные `field_id` операций и их вложенных агрегатов в `field_id_max`.
#[must_use]
pub const fn fits_field_id_space(desc: &ProtocolDesc, field_id_max: u8) -> bool {
    let mut operations = desc.operations;
    while let [operation, tail @ ..] = operations {
        let mut fields = operation.fields;
        while let [field, others @ ..] = fields {
            if field.field_id > field_id_max
                || !type_fits_field_id_space(field.field_type, field_id_max)
            {
                return false;
            }
            fields = others;
        }
        operations = tail;
    }
    true
}

/// Умещаются ли позиционные `field_id` агрегата и его вложенных агрегатов в `field_id_max`.
#[must_use]
pub const fn type_fits_field_id_space(field_type: WireType, field_id_max: u8) -> bool {
    let WireType::Aggregate(fields) = field_type else {
        return true;
    };
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

#[cfg(test)]
mod tests {
    use super::{
        FieldDesc, Kind, OperationDesc, ProtocolDesc, WireType, fits_body_budget,
        fits_field_id_space, is_one_way, max_data_len,
    };

    const FIELD_ID_MAX: u8 = 254;

    const fn operation(kind: Kind, fields: &'static [FieldDesc]) -> OperationDesc {
        OperationDesc {
            name: "op",
            canonical: "Proto.op",
            ordinal: 0,
            kind,
            fields,
        }
    }

    #[test]
    fn aggregate_data_length_counts_its_own_records() {
        // Терминатор суб-кадра плюс две записи по три байта накладных расходов.
        assert_eq!(
            max_data_len(WireType::Aggregate(&[WireType::Uint(4), WireType::Bool])),
            1 + (3 + 4) + (3 + 1)
        );
    }

    #[test]
    fn body_budget_counts_every_field_of_the_widest_operation() {
        const NARROW: [FieldDesc; 1] = [FieldDesc {
            field_id: 1,
            field_type: WireType::BoundedStr(16),
        }];
        const WIDE: [FieldDesc; 2] = [
            FieldDesc {
                field_id: 1,
                field_type: WireType::BoundedStr(16),
            },
            FieldDesc {
                field_id: 2,
                field_type: WireType::BoundedBytes(16),
            },
        ];
        const DESC: ProtocolDesc = ProtocolDesc {
            operations: &[operation(Kind::Cast, &NARROW), operation(Kind::Cast, &WIDE)],
        };

        assert!(fits_body_budget(&DESC, 1 + 2 * (3 + 16)));
        assert!(!fits_body_budget(&DESC, 1 + 2 * (3 + 16) - 1));
    }

    #[test]
    fn one_way_holds_until_a_two_way_operation_appears() {
        const ONE_WAY: ProtocolDesc = ProtocolDesc {
            operations: &[operation(Kind::Cast, &[]), operation(Kind::Event, &[])],
        };
        const TWO_WAY: ProtocolDesc = ProtocolDesc {
            operations: &[operation(Kind::Cast, &[]), operation(Kind::Call, &[])],
        };

        assert!(is_one_way(&ONE_WAY));
        assert!(!is_one_way(&TWO_WAY));
    }

    #[test]
    fn field_id_beyond_the_frame_space_is_rejected() {
        const RESERVED: [FieldDesc; 1] = [FieldDesc {
            field_id: FIELD_ID_MAX + 1,
            field_type: WireType::Bool,
        }];
        const LAST_ALLOWED: [FieldDesc; 1] = [FieldDesc {
            field_id: FIELD_ID_MAX,
            field_type: WireType::Bool,
        }];

        const BEYOND: ProtocolDesc = ProtocolDesc {
            operations: &[operation(Kind::Cast, &RESERVED)],
        };
        const WITHIN: ProtocolDesc = ProtocolDesc {
            operations: &[operation(Kind::Cast, &LAST_ALLOWED)],
        };

        assert!(!fits_field_id_space(&BEYOND, FIELD_ID_MAX));
        assert!(fits_field_id_space(&WITHIN, FIELD_ID_MAX));
    }

    #[test]
    fn aggregate_wider_than_the_frame_space_is_rejected() {
        const WIDE: [WireType; 3] = [WireType::Bool, WireType::Bool, WireType::Bool];
        const NESTED: [FieldDesc; 1] = [FieldDesc {
            field_id: 1,
            field_type: WireType::Aggregate(&[WireType::Aggregate(&WIDE)]),
        }];
        const DESC: ProtocolDesc = ProtocolDesc {
            operations: &[operation(Kind::Cast, &NESTED)],
        };

        // Агрегат нумерует поля так же позиционно, и вложенный считается наравне.
        assert!(fits_field_id_space(&DESC, 3));
        assert!(!fits_field_id_space(&DESC, 2));
    }
}
