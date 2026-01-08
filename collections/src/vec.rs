use core::ops::{Index, IndexMut};

pub struct Vec<T, const N: usize> {
    items: [Option<T>; N],
    len: usize,
}

impl<T, const N: usize> Vec<T, N> {
    pub fn new() -> Self {
        Self {
            items: core::array::from_fn(|_| None),
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }

        self.items[index].as_ref()
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

    pub fn sort_unstable_by_key<K, F>(&mut self, mut key: F)
    where
        F: FnMut(&T) -> K,
        K: Ord,
    {
        let slice = &mut self.items[..self.len];

        slice.sort_unstable_by(|a, b| {
            // по инварианту в [0..len) всегда Some
            let a = a.as_ref().unwrap();
            let b = b.as_ref().unwrap();

            key(a).cmp(&key(b))
        });
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
}

impl<T, const N: usize> Index<usize> for Vec<T, N> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        if index >= self.len {
            panic!("index out of bounds: index {} >= len {}", index, self.len);
        }

        self.items[index].as_ref().unwrap()
    }
}

impl<T, const N: usize> IndexMut<usize> for Vec<T, N> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        if index >= self.len {
            panic!("index out of bounds: index {} >= len {}", index, self.len);
        }

        self.items[index].as_mut().unwrap()
    }
}

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

impl<'a, T> ExactSizeIterator for VecIter<'a, T> {
    fn len(&self) -> usize {
        self.len - self.index
    }
}

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

impl <'a, T, const N: usize> IntoIterator for &'a Vec<T, N> {
    type Item = &'a T;
    type IntoIter = VecIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T, const N: usize> FromIterator<T> for Vec<T, N> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut vec = Self::new();

        for item in iter {
            vec.push(item).unwrap();
        }

        vec
    }
}
