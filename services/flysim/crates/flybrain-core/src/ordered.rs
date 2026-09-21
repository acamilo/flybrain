//! Insertion-ordered string-keyed number maps.
//!
//! Several pieces of exported state are JavaScript `Record<string, number>`s whose *key order* is
//! observable: it decides the order of `JSON.stringify` output in a checkpoint manifest, and the
//! `rates` record's order is the network's tracked-role order. [`NumberMap`] is an
//! insertion-ordered map with JavaScript `Object.assign` semantics, so those orders survive the
//! port.

use indexmap::IndexMap;

use crate::json::JsonValue;

/// An insertion-ordered `Record<string, number>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NumberMap {
    entries: IndexMap<String, f64>,
}

impl NumberMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or update. An existing key keeps its position, as in JavaScript.
    pub fn set(&mut self, key: &str, value: f64) {
        self.entries.insert(key.to_string(), value);
    }

    pub fn get(&self, key: &str) -> Option<f64> {
        self.entries.get(key).copied()
    }

    /// `record[key] ?? 0`, the lookup the decoder and the kernel use for absent roles.
    pub fn get_or_zero(&self, key: &str) -> f64 {
        self.get(key).unwrap_or(0.0)
    }

    /// Drop `key`, and report whether it was there.
    ///
    /// Only the tests need it — a decoder never forgets a baseline in a run — and what they need it
    /// for is building the state a checkpoint written before a channel group looks like
    /// (`docs/readout.md`, "Channels added after a checkpoint was written").
    pub fn remove(&mut self, key: &str) -> bool {
        self.entries.shift_remove(key).is_some()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = f64> + '_ {
        self.entries.values().copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, f64)> {
        self.entries.iter().map(|(key, value)| (key, *value))
    }

    /// `Object.assign(this, other)`: update existing keys in place, append new ones in `other`'s
    /// order.
    pub fn assign(&mut self, other: &NumberMap) {
        for (key, value) in other.iter() {
            self.entries.insert(key.clone(), value);
        }
    }

    pub fn from_pairs<I, K>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, f64)>,
        K: AsRef<str>,
    {
        let mut map = Self::new();
        for (key, value) in pairs {
            map.set(key.as_ref(), value);
        }
        map
    }
}

impl<'a> IntoIterator for &'a NumberMap {
    type Item = (&'a String, f64);
    type IntoIter = Box<dyn Iterator<Item = (&'a String, f64)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl NumberMap {
    /// The map as a JSON object, in insertion order.
    pub fn to_json(&self) -> JsonValue {
        JsonValue::Object(
            self.iter()
                .map(|(key, value)| (key.clone(), JsonValue::Number(value)))
                .collect(),
        )
    }

    /// Read a JSON object back into a map, rejecting any non-numeric member.
    pub fn from_json(value: &JsonValue) -> Option<Self> {
        let mut map = Self::new();
        for (key, member) in value.as_object()? {
            map.set(key, member.as_f64()?);
        }
        Some(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_keeps_existing_positions_and_appends_new_keys() {
        let mut base = NumberMap::from_pairs([("a", 1.0), ("b", 2.0)]);
        base.assign(&NumberMap::from_pairs([("b", 9.0), ("c", 3.0)]));
        assert_eq!(
            base.keys().cloned().collect::<Vec<_>>(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(base.get("b"), Some(9.0));
    }
}
