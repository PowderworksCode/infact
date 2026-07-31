use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};

pub fn into_group_map_with_hasher<I, K, V, S>(
    iter: I,
    hash_builder: S,
) -> HashMap<K, Vec<V>, S>
where
    I: Iterator<Item = (K, V)>,
    K: Hash + Eq,
    S: BuildHasher,
{
    let mut lookup = HashMap::<K, Vec<V>, S>::with_hasher(hash_builder);
    iter.for_each(|(key, value)| {
        lookup.entry(key).or_default().push(value);
    });
    lookup
}

pub fn into_group_map_by_with_hasher<I, K, V, F, S>(
    iter: I,
    mut f: F,
    hash_builder: S,
) -> HashMap<K, Vec<V>, S>
where
    I: Iterator<Item = V>,
    K: Hash + Eq,
    F: FnMut(&V) -> K,
    S: BuildHasher,
{
    into_group_map_with_hasher(iter.map(|value| (f(&value), value)), hash_builder)
}
