//! Exact copy-on-write sample storage. Cloning a view never copies its sample buffers.
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(transparent)]
pub struct SharedVec<T>(Arc<Vec<T>>);
impl<T> Default for SharedVec<T> {
    fn default() -> Self {
        Vec::new().into()
    }
}
impl<T> From<Vec<T>> for SharedVec<T> {
    fn from(v: Vec<T>) -> Self {
        Self(Arc::new(v))
    }
}
impl<T> FromIterator<T> for SharedVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(i: I) -> Self {
        i.into_iter().collect::<Vec<_>>().into()
    }
}
impl<T> Deref for SharedVec<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.0
    }
}
impl<T: Clone> DerefMut for SharedVec<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        Arc::make_mut(&mut self.0)
    }
}
impl<T: PartialEq> PartialEq<Vec<T>> for SharedVec<T> {
    fn eq(&self, other: &Vec<T>) -> bool {
        **self == *other
    }
}
impl<T: PartialEq> PartialEq<SharedVec<T>> for Vec<T> {
    fn eq(&self, other: &SharedVec<T>) -> bool {
        *self == **other
    }
}
impl<T: PartialEq, const N: usize> PartialEq<[T; N]> for SharedVec<T> {
    fn eq(&self, other: &[T; N]) -> bool {
        self.as_slice() == other
    }
}
impl<'a, T> IntoIterator for &'a SharedVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<'a, T: Clone> IntoIterator for &'a mut SharedVec<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}
impl<T: Clone> IntoIterator for SharedVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        Arc::unwrap_or_clone(self.0).into_iter()
    }
}
impl<T> SharedVec<T> {
    pub fn shares_storage(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
#[cfg(test)]
mod tests {
    use super::SharedVec;
    #[test]
    fn cow_retains_exact_bits_and_detaches_only_on_mutation() {
        let bits = [0x80000000, 0x7fc01234, 0x3f800001];
        let original: SharedVec<f32> = bits.map(f32::from_bits).to_vec().into();
        let mut edit = original.clone();
        assert!(edit.shares_storage(&original));
        assert_eq!(edit.iter().map(|v| v.to_bits()).collect::<Vec<_>>(), bits);
        edit[2] = 2.0;
        assert!(!edit.shares_storage(&original));
        assert_eq!(
            original.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            bits
        );
    }
}
