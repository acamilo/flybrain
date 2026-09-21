//! Fixed-size bitsets over dense index spaces.
//!
//! Two of the kernel's hot loops ask a yes/no question about a very sparse subset of a very large
//! index space — "is this one of the 16,384 plastic edges, out of 2.7 million?" and "did this
//! neuron spike this tick?" — and the answer used to cost a random read from an array with one
//! entry per element. A bitset answers the same question from 1/32nd of the memory, which is the
//! difference between evicting the membrane arrays from L2 and not.
//!
//! Nothing here changes a single arithmetic result; [`RankedBitSet`] returns exactly the slot index
//! the dense `Int32Array` it replaces used to hold.

/// A bitset over `0..bits`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BitSet {
    words: Vec<u64>,
    bits: usize,
}

impl BitSet {
    pub fn new(bits: usize) -> Self {
        Self {
            words: vec![0; bits.div_ceil(64)],
            bits,
        }
    }

    pub fn len(&self) -> usize {
        self.bits
    }

    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }

    #[inline]
    pub fn insert(&mut self, index: usize) {
        self.words[index >> 6] |= 1u64 << (index & 63);
    }

    #[inline]
    pub fn contains(&self, index: usize) -> bool {
        self.words[index >> 6] & (1u64 << (index & 63)) != 0
    }

    /// Clear every bit. `O(bits / 64)`, which for the spike set is 2 KB of stores.
    pub fn clear(&mut self) {
        self.words.fill(0);
    }

    pub fn count(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    /// The raw words, so a hot loop can hoist one word across a run of neighbouring indices.
    #[inline]
    pub fn words(&self) -> &[u64] {
        &self.words
    }
}

/// A [`BitSet`] plus a prefix-popcount table, so a set bit can name its position among the set
/// bits — the replacement for a dense "index -> slot, or -1" array.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RankedBitSet {
    set: BitSet,
    /// `rank[w]` is the number of set bits in words `0..w`.
    rank: Vec<u32>,
}

impl RankedBitSet {
    /// Build from ascending, distinct indices. Element `k` of `indices` becomes rank `k`.
    pub fn from_ascending(bits: usize, indices: impl IntoIterator<Item = usize>) -> Self {
        let mut set = BitSet::new(bits);
        for index in indices {
            set.insert(index);
        }
        let mut rank = Vec::with_capacity(set.words.len() + 1);
        let mut total = 0u32;
        for word in &set.words {
            rank.push(total);
            total += word.count_ones();
        }
        rank.push(total);
        Self { set, rank }
    }

    #[inline]
    pub fn contains(&self, index: usize) -> bool {
        self.set.contains(index)
    }

    /// Rank of `index` among the set bits, or `None` when the bit is clear.
    ///
    /// The clear case — the overwhelming majority — touches one word of the bitset and nothing
    /// else; the rank table and anything it indexes stay cold.
    #[inline]
    pub fn rank_of(&self, index: usize) -> Option<u32> {
        let word = self.set.words[index >> 6];
        let bit = 1u64 << (index & 63);
        if word & bit == 0 {
            return None;
        }
        Some(self.rank[index >> 6] + (word & (bit - 1)).count_ones())
    }

    /// Rank of `index` given its already-loaded word, which the caller has checked is set.
    ///
    /// A loop walking consecutive indices — a CSR row — loads one word and then asks this, so the
    /// bitset costs one load per row rather than one per element.
    #[inline]
    pub fn rank_within(&self, index: usize, word: u64) -> u32 {
        let bit = 1u64 << (index & 63);
        self.rank[index >> 6] + (word & (bit - 1)).count_ones()
    }

    pub fn count(&self) -> usize {
        self.rank.last().copied().unwrap_or(0) as usize
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    #[inline]
    pub fn words(&self) -> &[u64] {
        self.set.words()
    }

    /// Bytes of heap this occupies, for the memory accounting in the crate README.
    pub fn heap_bytes(&self) -> usize {
        self.set.words.len() * 8 + self.rank.len() * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_match_a_dense_slot_array() {
        let indices = [0usize, 1, 63, 64, 65, 200, 4095, 4096, 9999];
        let ranked = RankedBitSet::from_ascending(10_000, indices.iter().copied());
        let mut dense = vec![-1i32; 10_000];
        for (slot, index) in indices.iter().enumerate() {
            dense[*index] = slot as i32;
        }
        for (index, want) in dense.iter().copied().enumerate() {
            let got = ranked.rank_of(index).map_or(-1i32, |rank| rank as i32);
            assert_eq!(got, want, "index {index}");
            // The hoisted-word form has to agree with the per-index form.
            let word = ranked.words()[index >> 6];
            let hoisted = if word & (1u64 << (index & 63)) == 0 {
                -1i32
            } else {
                ranked.rank_within(index, word) as i32
            };
            assert_eq!(hoisted, want, "hoisted index {index}");
        }
        assert_eq!(ranked.count(), indices.len());
    }

    #[test]
    fn an_empty_set_contains_nothing() {
        let ranked = RankedBitSet::from_ascending(129, std::iter::empty());
        assert_eq!(ranked.count(), 0);
        for index in 0..129 {
            assert_eq!(ranked.rank_of(index), None);
        }
    }

    #[test]
    fn a_bitset_round_trips_membership() {
        let mut set = BitSet::new(300);
        for index in (0..300).step_by(7) {
            set.insert(index);
        }
        for index in 0..300 {
            assert_eq!(set.contains(index), index % 7 == 0, "index {index}");
        }
        assert_eq!(set.count(), 300usize.div_ceil(7));
        set.clear();
        assert_eq!(set.count(), 0);
    }
}
