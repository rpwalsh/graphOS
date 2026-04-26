#![no_std]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.

extern crate alloc;

use alloc::vec::Vec;
use core::hash::{Hash, Hasher};

#[derive(Default)]
struct Fnv64 {
    state: u64,
}

impl Fnv64 {
    #[inline]
    fn new() -> Self {
        Self {
            state: 0xcbf29ce484222325,
        }
    }
}

impl Hasher for Fnv64 {
    #[inline]
    fn finish(&self) -> u64 {
        self.state
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= u64::from(b);
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }
}

#[derive(Clone)]
enum Bucket<K, V> {
    Empty,
    Deleted,
    Occupied(K, V),
}

pub struct GraphHashMap<K, V> {
    buckets: Vec<Bucket<K, V>>,
    len: usize,
    deleted: usize,
}

impl<K, V> Default for GraphHashMap<K, V>
where
    K: Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> GraphHashMap<K, V>
where
    K: Eq + Hash,
{
    pub fn new() -> Self {
        Self::with_capacity(8)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two().max(8);
        let mut buckets = Vec::with_capacity(cap);
        for _ in 0..cap {
            buckets.push(Bucket::Empty);
        }
        Self {
            buckets,
            len: 0,
            deleted: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.maybe_grow();
        self.insert_no_grow(key, value)
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        let idx = self.find_index(key)?;
        match &self.buckets[idx] {
            Bucket::Occupied(_, v) => Some(v),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let idx = self.find_index(key)?;
        match &mut self.buckets[idx] {
            Bucket::Occupied(_, v) => Some(v),
            _ => None,
        }
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let idx = self.find_index(key)?;
        let old = core::mem::replace(&mut self.buckets[idx], Bucket::Deleted);
        match old {
            Bucket::Occupied(_, v) => {
                self.len -= 1;
                self.deleted += 1;
                Some(v)
            }
            _ => None,
        }
    }

    pub fn retain<F>(&mut self, mut keep: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        for i in 0..self.buckets.len() {
            let should_delete = match &mut self.buckets[i] {
                Bucket::Occupied(k, v) => !keep(k, v),
                _ => false,
            };
            if should_delete {
                self.buckets[i] = Bucket::Deleted;
                self.len -= 1;
                self.deleted += 1;
            }
        }
        self.maybe_rehash_after_delete();
    }

    fn maybe_grow(&mut self) {
        let cap = self.buckets.len();
        let used = self.len + self.deleted;
        // Grow if occupancy+tombstones crosses 70%.
        if used * 10 >= cap * 7 {
            self.rehash(cap * 2);
        }
    }

    fn maybe_rehash_after_delete(&mut self) {
        let cap = self.buckets.len();
        if self.deleted * 4 > cap {
            self.rehash(cap);
        }
    }

    fn insert_no_grow(&mut self, key: K, value: V) -> Option<V> {
        let cap = self.buckets.len();
        let mut idx = (hash_key(&key) as usize) & (cap - 1);
        let mut first_deleted: Option<usize> = None;

        loop {
            match &mut self.buckets[idx] {
                Bucket::Empty => {
                    let target = first_deleted.unwrap_or(idx);
                    if first_deleted.is_some() {
                        self.deleted -= 1;
                    }
                    self.buckets[target] = Bucket::Occupied(key, value);
                    self.len += 1;
                    return None;
                }
                Bucket::Deleted => {
                    if first_deleted.is_none() {
                        first_deleted = Some(idx);
                    }
                }
                Bucket::Occupied(k, v) => {
                    if *k == key {
                        let old = core::mem::replace(v, value);
                        return Some(old);
                    }
                }
            }
            idx = (idx + 1) & (cap - 1);
        }
    }

    fn find_index(&self, key: &K) -> Option<usize> {
        let cap = self.buckets.len();
        let mut idx = (hash_key(key) as usize) & (cap - 1);
        let mut scanned = 0usize;

        while scanned < cap {
            match &self.buckets[idx] {
                Bucket::Empty => return None,
                Bucket::Deleted => {}
                Bucket::Occupied(k, _) if k == key => return Some(idx),
                Bucket::Occupied(_, _) => {}
            }
            idx = (idx + 1) & (cap - 1);
            scanned += 1;
        }
        None
    }

    fn rehash(&mut self, new_capacity: usize) {
        let new_cap = new_capacity.next_power_of_two().max(8);
        let mut next = Self::with_capacity(new_cap);

        for b in self.buckets.drain(..) {
            if let Bucket::Occupied(k, v) = b {
                let _ = next.insert_no_grow(k, v);
            }
        }

        *self = next;
    }
}

#[inline]
fn hash_key<K: Hash>(key: &K) -> u64 {
    let mut h = Fnv64::new();
    key.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::GraphHashMap;

    #[test]
    fn insert_get_overwrite_remove() {
        let mut m = GraphHashMap::<u64, i32>::new();
        assert_eq!(m.insert(7, 10), None);
        assert_eq!(m.get(&7), Some(&10));
        assert_eq!(m.insert(7, 11), Some(10));
        assert_eq!(m.get(&7), Some(&11));
        assert_eq!(m.remove(&7), Some(11));
        assert_eq!(m.get(&7), None);
    }

    #[test]
    fn retain_filters_values() {
        let mut m = GraphHashMap::<u64, i32>::new();
        for i in 0..32 {
            let _ = m.insert(i, i as i32);
        }
        m.retain(|k, _| *k % 2 == 0);
        for i in 0..32 {
            if i % 2 == 0 {
                assert!(m.get(&i).is_some());
            } else {
                assert!(m.get(&i).is_none());
            }
        }
    }
}
