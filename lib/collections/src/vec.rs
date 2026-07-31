use core::ops::{Index, IndexMut};

/// Вектор фиксированной ёмкости без аллокаций.
///
/// Хранит до `N` элементов на стеке. Используется в no_std окружении
/// как замена `alloc::vec::Vec`.
#[derive(Clone, PartialEq, Eq)]
pub struct Vec<T, const N: usize> {
    /// Массив слотов для элементов.
    items: [Option<T>; N],

    /// Текущее количество элементов.
    len: usize,
}

impl<T, const N: usize> Vec<T, N> {
    pub const fn new() -> Self {
        Self {
            items: [const { None }; N],
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

impl<T, const N: usize> Default for Vec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Vec<T, N> {
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }

        self.items.get(index)?.as_ref()
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        if index >= self.len {
            return None;
        }

        self.items.get_mut(index)?.as_mut()
    }

    pub fn push(&mut self, item: T) -> Option<()> {
        if self.len == N {
            return None;
        }

        self.items[self.len] = Some(item);
        self.len += 1;

        Some(())
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }

        self.len -= 1;
        self.items[self.len].take()
    }

    pub fn truncate(&mut self, new_len: usize) {
        if new_len >= self.len {
            return;
        }

        // очищаем "хвост"
        for i in new_len..self.len {
            self.items[i].take();
        }

        self.len = new_len;
    }

    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Заменяет тип элементов, сохраняя порядок. Ёмкость результата та же,
    /// поэтому преобразование всегда полное.
    pub fn map_into<U>(self, mut map: impl FnMut(T) -> U) -> Vec<U, N> {
        let mut mapped: Vec<U, N> = Vec::new();

        for item in self {
            mapped.items[mapped.len] = Some(map(item));
            mapped.len += 1;
        }

        mapped
    }
}

impl<T, const N: usize> Index<usize> for Vec<T, N> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        let len = self.len;
        match self.get(index) {
            Some(item) => item,
            None => panic!("index out of bounds: index {index} >= len {len}"),
        }
    }
}

impl<T, const N: usize> IndexMut<usize> for Vec<T, N> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        let len = self.len;
        match self.get_mut(index) {
            Some(item) => item,
            None => panic!("index out of bounds: index {index} >= len {len}"),
        }
    }
}

/// Итератор по ссылкам на элементы вектора.
pub struct VecIter<'a, T> {
    items: &'a [Option<T>],
    len: usize,
    index: usize,
}

impl<T, const N: usize> Vec<T, N> {
    pub fn iter(&self) -> VecIter<'_, T> {
        VecIter {
            items: &self.items,
            len: self.len,
            index: 0,
        }
    }
}

impl<'a, T> Iterator for VecIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.len {
            None
        } else {
            let index = self.index;
            self.index += 1;
            self.items[index].as_ref()
        }
    }
}

impl<T> ExactSizeIterator for VecIter<'_, T> {
    fn len(&self) -> usize {
        self.len - self.index
    }
}

/// Итератор, потребляющий вектор и возвращающий элементы по значению.
pub struct VecIntoIter<T, const N: usize> {
    vec: Vec<T, N>,
    index: usize,
}

impl<T, const N: usize> Iterator for VecIntoIter<T, N> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.vec.len() {
            None
        } else {
            let item = self.vec.items[self.index].take();
            self.index += 1;
            item
        }
    }
}

impl<T, const N: usize> ExactSizeIterator for VecIntoIter<T, N> {
    fn len(&self) -> usize {
        self.vec.len() - self.index
    }
}

impl<T, const N: usize> IntoIterator for Vec<T, N> {
    type Item = T;
    type IntoIter = VecIntoIter<T, N>;

    fn into_iter(self) -> Self::IntoIter {
        VecIntoIter {
            vec: self,
            index: 0,
        }
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a Vec<T, N> {
    type Item = &'a T;
    type IntoIter = VecIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_into_keeps_order_of_elements() {
        let mut vec: Vec<u8, 4> = Vec::new();
        vec.push(1);
        vec.push(2);
        vec.push(3);

        let mapped: Vec<u32, 4> = vec.map_into(|item| u32::from(item) * 10);

        assert_eq!(mapped.len(), 3);
        assert_eq!(mapped[0], 10);
        assert_eq!(mapped[1], 20);
        assert_eq!(mapped[2], 30);
    }

    #[test]
    fn map_into_carries_every_element_of_a_full_vec() {
        let mut vec: Vec<u8, 3> = Vec::new();
        vec.push(7);
        vec.push(8);
        vec.push(9);

        let mapped: Vec<u8, 3> = vec.map_into(|item| item);

        assert_eq!(mapped.len(), 3);
        assert_eq!(mapped[2], 9);
    }
}
