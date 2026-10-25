use component_abi::{
    ATTRIBUTE_KEY_MAX_BYTES, ATTRIBUTE_VALUE_MAX_BYTES, Attribute, Attributes, attributes_len,
    encode_attributes_into,
};

const PAIR_MAX_BYTES: usize = attributes_len(&[Attribute {
    key: &[0; ATTRIBUTE_KEY_MAX_BYTES],
    value: &[0; ATTRIBUTE_VALUE_MAX_BYTES],
}]);

/// Запрос перечисления [`Providers::all`](crate::Providers::all) задаёт наборы
/// атрибутов, которые перечисление проходит по очереди. [`Attributes`] задаёт один набор.
pub trait Query {
    /// Набор шага `step`. Перечисление вызывает шаги подряд с 0, повторяет шаг после отказа
    /// `NoSpace` и заканчивается на первом `None`.
    fn attributes(&mut self, step: usize) -> Option<Attributes<'_>>;
}

/// Запрос пары `key = value` для каждого из `values` по порядку. Значение длиннее
/// `ATTRIBUTE_VALUE_MAX_BYTES` пропускается, ключ пустой либо длиннее
/// `ATTRIBUTE_KEY_MAX_BYTES` не даёт наборов.
pub struct AnyOf<'k, I> {
    key: &'k str,
    values: I,
    bytes: [u8; PAIR_MAX_BYTES],
    encoded: Option<(usize, usize)>,
}

impl<'k, 'v, I: Iterator<Item = &'v str>> AnyOf<'k, I> {
    #[must_use]
    pub fn new(key: &'k str, values: impl IntoIterator<IntoIter = I>) -> Self {
        Self {
            key,
            values: values.into_iter(),
            bytes: [0; PAIR_MAX_BYTES],
            encoded: None,
        }
    }
}

impl Query for Attributes<'_> {
    fn attributes(&mut self, step: usize) -> Option<Attributes<'_>> {
        (step == 0).then_some(*self)
    }
}

impl<'v, I: Iterator<Item = &'v str>> Query for AnyOf<'_, I> {
    fn attributes(&mut self, step: usize) -> Option<Attributes<'_>> {
        if self.key.is_empty() || self.key.len() > ATTRIBUTE_KEY_MAX_BYTES {
            return None;
        }

        let len = match self.encoded {
            Some((encoded, len)) if encoded == step => len,
            _ => {
                let value = self
                    .values
                    .find(|value| value.len() <= ATTRIBUTE_VALUE_MAX_BYTES)?;
                let pair = [Attribute {
                    key: self.key.as_bytes(),
                    value: value.as_bytes(),
                }];
                let len = attributes_len(&pair);
                encode_attributes_into(self.bytes.get_mut(..len)?, &pair);
                self.encoded = Some((step, len));
                len
            }
        };
        Attributes::decode(self.bytes.get(..len)?)
    }
}
