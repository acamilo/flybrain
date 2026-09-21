//! An insertion-ordered string set.
//!
//! The prototype's reward state is a set of `string` keys, and a JavaScript
//! `Set` iterates in insertion order, so its checkpoints record keys in the
//! order they were first earned. Reproducing that keeps an exported Rust
//! checkpoint comparable to the TypeScript one key by key.

use std::collections::HashSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderedSet {
    order: Vec<String>,
    index: HashSet<String>,
}

impl OrderedSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert if absent. Returns whether the set changed.
    pub fn insert(&mut self, key: &str) -> bool {
        if self.index.contains(key) {
            return false;
        }
        self.index.insert(key.to_string());
        self.order.push(key.to_string());
        true
    }

    pub fn contains(&self, key: &str) -> bool {
        self.index.contains(key)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Kept for symmetry with [`OrderedSet::len`] (`clippy::len_without_is_empty`).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Keys in insertion order.
    pub fn as_slice(&self) -> &[String] {
        &self.order
    }
}

impl<'a> FromIterator<&'a str> for OrderedSet {
    fn from_iter<I: IntoIterator<Item = &'a str>>(keys: I) -> Self {
        let mut set = Self::new();
        for key in keys {
            set.insert(key);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_order_is_preserved_and_duplicates_are_dropped() {
        let mut set = OrderedSet::new();
        assert!(set.insert("b"));
        assert!(set.insert("a"));
        assert!(!set.insert("b"));
        assert_eq!(set.as_slice(), ["b", "a"]);
        assert_eq!(set.len(), 2);
        assert!(set.contains("a"));
        assert!(!set.contains("c"));
    }
}
