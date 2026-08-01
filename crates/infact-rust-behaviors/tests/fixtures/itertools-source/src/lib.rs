use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, RandomState};

trait Itertools: Iterator {
    fn map_into<R>(self) -> MapInto<Self, R>
    where
        Self: Sized,
        Self::Item: Into<R>,
    {
        map_into(self)
    }

    fn counts(self) -> HashMap<Self::Item, usize>
    where
        Self: Sized,
        Self::Item: Eq + Hash,
    {
        self.counts_with_hasher(RandomState::new())
    }

    fn counts_with_hasher<S>(self, hash_builder: S) -> HashMap<Self::Item, usize, S>
    where
        Self: Sized,
        Self::Item: Eq + Hash,
        S: BuildHasher,
    {
        let mut counts = HashMap::with_hasher(hash_builder);
        self.for_each(|item| *counts.entry(item).or_default() += 1);
        counts
    }

    fn counts_by<K, F>(self, f: F) -> HashMap<K, usize>
    where
        Self: Sized,
        K: Eq + Hash,
        F: FnMut(Self::Item) -> K,
    {
        self.counts_by_with_hasher(f, RandomState::new())
    }

    fn counts_by_with_hasher<K, F, S>(
        self,
        f: F,
        hash_builder: S,
    ) -> HashMap<K, usize, S>
    where
        Self: Sized,
        K: Eq + Hash,
        F: FnMut(Self::Item) -> K,
        S: BuildHasher,
    {
        self.map(f).counts_with_hasher(hash_builder)
    }

    fn into_group_map<K, V>(self) -> HashMap<K, Vec<V>>
    where
        Self: Iterator<Item = (K, V)> + Sized,
        K: Hash + Eq,
    {
        group_map::into_group_map_with_hasher(self, RandomState::new())
    }

    fn into_group_map_by<K, V, F>(self, f: F) -> HashMap<K, Vec<V>>
    where
        Self: Iterator<Item = V> + Sized,
        K: Hash + Eq,
        F: FnMut(&V) -> K,
    {
        group_map::into_group_map_by_with_hasher(self, f, RandomState::new())
    }

    fn sorted(self) -> std::vec::IntoIter<Self::Item>
    where
        Self: Sized,
        Self::Item: Ord,
    {
        let mut v = Vec::from_iter(self);
        v.sort();
        v.into_iter()
    }

    fn sorted_by<F>(self, cmp: F) -> std::vec::IntoIter<Self::Item>
    where
        Self: Sized,
        F: FnMut(&Self::Item, &Self::Item) -> std::cmp::Ordering,
    {
        let mut v = Vec::from_iter(self);
        v.sort_by(cmp);
        v.into_iter()
    }

    fn sorted_by_key<K, F>(self, f: F) -> std::vec::IntoIter<Self::Item>
    where
        Self: Sized,
        K: Ord,
        F: FnMut(&Self::Item) -> K,
    {
        let mut v = Vec::from_iter(self);
        v.sort_by_key(f);
        v.into_iter()
    }

    fn sorted_unstable(self) -> std::vec::IntoIter<Self::Item>
    where
        Self: Sized,
        Self::Item: Ord,
    {
        let mut v = Vec::from_iter(self);
        v.sort_unstable();
        v.into_iter()
    }

    fn sorted_unstable_by<F>(self, cmp: F) -> std::vec::IntoIter<Self::Item>
    where
        Self: Sized,
        F: FnMut(&Self::Item, &Self::Item) -> std::cmp::Ordering,
    {
        let mut v = Vec::from_iter(self);
        v.sort_unstable_by(cmp);
        v.into_iter()
    }

    fn sorted_unstable_by_key<K, F>(self, f: F) -> std::vec::IntoIter<Self::Item>
    where
        Self: Sized,
        K: Ord,
        F: FnMut(&Self::Item) -> K,
    {
        let mut v = Vec::from_iter(self);
        v.sort_unstable_by_key(f);
        v.into_iter()
    }
}

mod group_map;

/// An adaptor: the public method builds a value and returns immediately, and
/// the behavior runs later, in the type's iterator implementation.
pub struct MapInto<I, R> {
    iter: I,
    marker: std::marker::PhantomData<R>,
}

pub fn map_into<I, R>(iter: I) -> MapInto<I, R> {
    MapInto {
        iter,
        marker: std::marker::PhantomData,
    }
}

impl<I, R> Iterator for MapInto<I, R>
where
    I: Iterator,
    I::Item: Into<R>,
{
    type Item = R;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|item| item.into())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}
