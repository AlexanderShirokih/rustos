//! Наборы пар ключ-значение: атрибуты запроса и фильтры экспортов.

use core::fmt;

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::WireType,
    wire::{IpcError, MessageBuf},
};

use crate::manifest::Writer;

/// Наибольшее число пар в наборе.
pub const ATTRIBUTE_COUNT_MAX: usize = 8;
/// Наибольшая длина ключа в байтах.
pub const ATTRIBUTE_KEY_MAX_BYTES: usize = 64;
/// Наибольшая длина значения в байтах.
pub const ATTRIBUTE_VALUE_MAX_BYTES: usize = 255;
/// Наибольшая длина закодированного набора в байтах.
pub const ATTRIBUTES_MAX_BYTES: usize =
    1 + ATTRIBUTE_COUNT_MAX * (2 + ATTRIBUTE_KEY_MAX_BYTES + ATTRIBUTE_VALUE_MAX_BYTES);

const _: () = assert!(
    ATTRIBUTES_MAX_BYTES <= u16::MAX as usize,
    "attribute set length fits u16"
);

/// Пара ключ-значение набора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attribute<'a> {
    pub key: &'a [u8],
    pub value: &'a [u8],
}

/// Проверенный набор с числом пар до [`ATTRIBUTE_COUNT_MAX`] и уникальными непустыми ключами.
/// Длины ключа и значения ограничены [`ATTRIBUTE_KEY_MAX_BYTES`] и [`ATTRIBUTE_VALUE_MAX_BYTES`].
/// Пустое значение допустимо.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attributes<'a> {
    bytes: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct AttributePairs<'a> {
    bytes: &'a [u8],
}

/// Копия набора пар во владеющем буфере. Ответ IPC владеет своими значениями.
#[derive(Clone, PartialEq, Eq)]
pub struct AttributeSet {
    bytes: [u8; ATTRIBUTES_MAX_BYTES],
    len: u16,
}

impl<'a> Attributes<'a> {
    #[must_use]
    pub const fn empty() -> Self {
        Self { bytes: &[0] }
    }

    #[must_use]
    pub fn decode(bytes: &'a [u8]) -> Option<Self> {
        let (attributes, rest) = Self::decode_prefix(bytes)?;
        rest.is_empty().then_some(attributes)
    }

    pub(crate) fn decode_prefix(bytes: &'a [u8]) -> Option<(Self, &'a [u8])> {
        let (&count, mut rest) = bytes.split_first()?;
        if usize::from(count) > ATTRIBUTE_COUNT_MAX {
            return None;
        }

        let mut keys = [None; ATTRIBUTE_COUNT_MAX];
        for slot in 0..usize::from(count) {
            let (pair, tail) = read_pair(rest)?;
            if keys.iter().flatten().any(|key| *key == pair.key) {
                return None;
            }
            *keys.get_mut(slot)? = Some(pair.key);
            rest = tail;
        }

        let end = bytes.len() - rest.len();
        Some((
            Self {
                bytes: bytes.get(..end)?,
            },
            rest,
        ))
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.bytes == [0]
    }

    #[must_use]
    pub const fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }

    #[must_use]
    pub fn iter(self) -> AttributePairs<'a> {
        AttributePairs {
            bytes: self.bytes.get(1..).unwrap_or_default(),
        }
    }

    /// Значение пары с ключом `key`.
    #[must_use]
    pub fn value(self, key: &[u8]) -> Option<&'a [u8]> {
        self.iter()
            .find(|pair| pair.key == key)
            .map(|pair| pair.value)
    }
}

impl AttributeSet {
    #[must_use]
    pub fn new(attributes: Attributes<'_>) -> Self {
        let source = attributes.as_bytes();
        let mut bytes = [0; ATTRIBUTES_MAX_BYTES];
        if let Some(target) = bytes.get_mut(..source.len()) {
            target.copy_from_slice(source);
        }

        Self {
            bytes,
            len: u16::try_from(source.len()).unwrap_or_default(),
        }
    }

    #[must_use]
    pub fn as_attributes(&self) -> Attributes<'_> {
        Attributes {
            bytes: self.bytes.get(..usize::from(self.len)).unwrap_or(&[0]),
        }
    }
}

impl fmt::Debug for AttributeSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_list()
            .entries(self.as_attributes().iter())
            .finish()
    }
}

impl<'a> Iterator for AttributePairs<'a> {
    type Item = Attribute<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let (pair, rest) = read_pair(self.bytes)?;
        self.bytes = rest;
        Some(pair)
    }
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for Attributes<'a> {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Self::decode(data).ok_or(IpcError::InvalidValue)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for Attributes<'_> {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, self.bytes)
    }
}

impl WireTyped for Attributes<'_> {
    const WIRE_TYPE: WireType = WireType::BoundedBytes(ATTRIBUTES_MAX_BYTES);
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for AttributeSet {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Attributes::decode(data)
            .map(Self::new)
            .ok_or(IpcError::InvalidValue)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for AttributeSet {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, self.as_attributes().as_bytes())
    }
}

impl WireTyped for AttributeSet {
    const WIRE_TYPE: WireType = WireType::BoundedBytes(ATTRIBUTES_MAX_BYTES);
}

#[must_use]
pub const fn attributes_len(pairs: &[Attribute<'_>]) -> usize {
    let mut counter = Writer::counter();
    write_attributes(&mut counter, pairs);
    counter.len
}

/// Кодирует пары в буфер длиной `attributes_len`. Длины и число пар должны быть допустимы.
pub const fn encode_attributes_into(bytes: &mut [u8], pairs: &[Attribute<'_>]) {
    assert!(
        bytes.len() == attributes_len(pairs),
        "attribute buffer has the declared length"
    );
    write_attributes(&mut Writer::new(bytes), pairs);
}

pub(crate) const fn write_attributes(out: &mut Writer<'_>, pairs: &[Attribute<'_>]) {
    assert!(
        pairs.len() <= ATTRIBUTE_COUNT_MAX,
        "attribute count fits the limit"
    );
    out.put_len(pairs.len());

    let mut pending = pairs;
    while let [pair, tail @ ..] = pending {
        assert!(
            !pair.key.is_empty() && pair.key.len() <= ATTRIBUTE_KEY_MAX_BYTES,
            "attribute key fits the limit"
        );
        assert!(
            pair.value.len() <= ATTRIBUTE_VALUE_MAX_BYTES,
            "attribute value fits the limit"
        );

        out.put_len(pair.key.len());
        out.put(pair.key);
        out.put_len(pair.value.len());
        out.put(pair.value);
        pending = tail;
    }
}

/// Возвращает первый совпавший фильтр. Экспорт без фильтров совпадает только с пустым запросом.
#[must_use]
pub fn match_filters<'a>(
    filters: impl IntoIterator<Item = Attributes<'a>>,
    query: Attributes<'_>,
) -> Option<Attributes<'a>> {
    let mut filters = filters.into_iter().peekable();
    if filters.peek().is_none() {
        return query.is_empty().then_some(Attributes::empty());
    }

    filters.find(|filter| {
        !filter.is_empty()
            && filter
                .iter()
                .all(|pair| query.iter().any(|candidate| candidate == pair))
    })
}

fn read_pair(bytes: &[u8]) -> Option<(Attribute<'_>, &[u8])> {
    let (&key_len, rest) = bytes.split_first()?;
    let key_len = usize::from(key_len);
    if key_len == 0 || key_len > ATTRIBUTE_KEY_MAX_BYTES {
        return None;
    }
    let key = rest.get(..key_len)?;

    let (&value_len, rest) = rest.get(key_len..)?.split_first()?;
    let value_len = usize::from(value_len);
    let value = rest.get(..value_len)?;

    Some((Attribute { key, value }, rest.get(value_len..)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(pairs: &[Attribute<'_>]) -> std::vec::Vec<u8> {
        let mut bytes = std::vec![0; attributes_len(pairs)];
        encode_attributes_into(&mut bytes, pairs);
        bytes
    }

    fn pair(key: &'static [u8], value: &'static [u8]) -> Attribute<'static> {
        Attribute { key, value }
    }

    #[test]
    fn filters_require_all_pairs_and_choose_the_first_matching_alternative() {
        let first = encoded(&[pair(b"compatible", b"A"), pair(b"role", b"B")]);
        let second = encoded(&[pair(b"compatible", b"A")]);
        let query = encoded(&[
            pair(b"extra", b"C"),
            pair(b"role", b"B"),
            pair(b"compatible", b"A"),
        ]);
        let first = Attributes::decode(&first).unwrap();
        let second = Attributes::decode(&second).unwrap();
        let query = Attributes::decode(&query).unwrap();

        assert_eq!(match_filters([first, second], query), Some(first));
        assert_eq!(match_filters([first], second), None);
        assert_eq!(match_filters([first, second], second), Some(second));
    }

    #[test]
    fn empty_value_is_distinct_from_missing_key_and_bytes_need_not_be_utf8() {
        let bytes = encoded(&[pair(b"\xff", b"")]);
        let filter = Attributes::decode(&bytes).unwrap();

        assert_eq!(match_filters([filter], Attributes::empty()), None);
        assert_eq!(match_filters([filter], filter), Some(filter));
    }

    #[test]
    fn an_export_without_filters_matches_only_an_empty_query() {
        let bytes = encoded(&[pair(b"compatible", b"A")]);

        assert_eq!(
            match_filters([], Attributes::empty()),
            Some(Attributes::empty())
        );
        assert_eq!(match_filters([], Attributes::decode(&bytes).unwrap()), None);
    }

    #[test]
    fn invalid_keys_counts_lengths_and_trailing_bytes_are_refused() {
        for bytes in [
            std::vec![1, 0, 0],
            std::vec![9],
            std::vec![1, 65],
            std::vec![0, 0],
            std::vec![],
        ] {
            assert_eq!(Attributes::decode(&bytes), None);
        }

        let duplicate = encoded(&[pair(b"k", b"A"), pair(b"k", b"B")]);
        assert_eq!(Attributes::decode(&duplicate), None);

        let bytes = encoded(&[pair(b"k", b"A")]);
        for len in 0..bytes.len() {
            assert_eq!(Attributes::decode(&bytes[..len]), None);
        }
    }

    #[test]
    fn maximal_pairs_round_trip_at_the_encoded_bound() {
        let keys: std::vec::Vec<_> = (0..ATTRIBUTE_COUNT_MAX)
            .map(|i| std::vec![u8::try_from(i).unwrap(); ATTRIBUTE_KEY_MAX_BYTES])
            .collect();
        let value = [0xff; ATTRIBUTE_VALUE_MAX_BYTES];
        let pairs: std::vec::Vec<_> = keys
            .iter()
            .map(|key| Attribute { key, value: &value })
            .collect();
        let bytes = encoded(&pairs);

        let attributes = Attributes::decode(&bytes).unwrap();

        assert_eq!(attributes.iter().collect::<std::vec::Vec<_>>(), pairs);
        assert_eq!(bytes.len(), ATTRIBUTES_MAX_BYTES);
    }
}
