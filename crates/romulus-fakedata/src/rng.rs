//! Seeded randomness. Every value is drawn from a `ChaCha8` RNG keyed by
//! `sha256(seed ‖ schema ‖ table ‖ column)` (docs/playground.md,
//! "Determinism"), on the stream of its row: a cell is a pure function of
//! (seed, table, column, row), so a column added leaves the others as they
//! were, and a key can be computed for any row without generating the rows
//! before it (foreign keys need exactly that).

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use sha2::{Digest, Sha256};

pub type Key = [u8; 32];

/// The key of one column (or one row-level decision: a foreign key's
/// choice, a partition, a combination).
pub fn key(seed: u64, parts: &[&str]) -> Key {
    let mut h = Sha256::new();
    h.update(seed.to_be_bytes());
    for part in parts {
        h.update([0]);
        h.update(part.as_bytes());
    }
    h.finalize().into()
}

/// The RNG of one cell. `attempt` is a redraw of the row (a CHECK the row
/// broke); rows stay below 2^48, so the attempt fits above them.
pub fn cell(key: &Key, row: u64, attempt: u32) -> ChaCha8Rng {
    let mut rng = ChaCha8Rng::from_seed(*key);
    rng.set_stream(row | (u64::from(attempt) << 48));
    rng
}

/// The 64-bit finaliser of `SplitMix64`: a good mix of one word.
pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// A bijection of `0..n`, keyed: a Feistel network over the next even
/// power of two, walking cycles back into range. `apply(i)` for `i < n`
/// lists every value of `0..n` exactly once, in a scrambled order: keys
/// unique by construction, "1:1" foreign keys sampled without replacement.
#[derive(Debug, Clone)]
pub struct Permutation {
    n: u64,
    half: u32,
    keys: [u64; 4],
}

impl Permutation {
    pub fn new(n: u64, key: &Key) -> Self {
        let bits = (64 - n.saturating_sub(1).leading_zeros()).max(2);
        let bits = bits + bits % 2;
        let word =
            |i: usize| u64::from_le_bytes(key[i * 8..i * 8 + 8].try_into().unwrap_or([0; 8]));
        Self {
            n,
            half: bits / 2,
            keys: [word(0), word(1), word(2), word(3)],
        }
    }

    pub fn len(&self) -> u64 {
        self.n
    }

    pub fn apply(&self, i: u64) -> u64 {
        if self.n <= 1 {
            return 0;
        }
        let mut x = i % self.n;
        loop {
            x = self.round(x);
            if x < self.n {
                return x;
            }
        }
    }

    fn round(&self, x: u64) -> u64 {
        let mask = if self.half >= 32 {
            u64::from(u32::MAX)
        } else {
            (1u64 << self.half) - 1
        };
        let (mut l, mut r) = ((x >> self.half) & mask, x & mask);
        for k in self.keys {
            let f = mix(r ^ k) & mask;
            (l, r) = (r, l ^ f);
        }
        (l << self.half) | r
    }
}

/// A skewed rank in `0..n`: rank `k` comes up about `1/(k+1)` as often as
/// rank 0 (Zipf with exponent 1), so a few parents get many children.
pub fn skewed(rng: &mut ChaCha8Rng, n: u64) -> u64 {
    if n <= 1 {
        return 0;
    }
    let u: f64 = rng.random();
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let rank = (libm::pow(n as f64 + 1.0, u) - 1.0).floor() as u64;
    rank.min(n - 1)
}

/// A standard normal draw (Box–Muller).
pub fn normal(rng: &mut ChaCha8Rng) -> f64 {
    let u1: f64 = rng.random::<f64>().max(f64::MIN_POSITIVE);
    let u2: f64 = rng.random();
    libm::sqrt(-2.0 * libm::log(u1)) * libm::cos(std::f64::consts::TAU * u2)
}

/// `10^e`, the same on every platform.
pub fn pow10(e: i32) -> f64 {
    libm::pow(10.0, f64::from(e))
}

/// Picks one item.
pub fn pick<'a, T>(rng: &mut ChaCha8Rng, items: &'a [T]) -> &'a T {
    &items[rng.random_range(0..items.len())]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn a_permutation_lists_every_value_once() {
        for n in [1, 2, 3, 10, 97, 1000, 4096, 5000] {
            let p = Permutation::new(n, &key(7, &["t"]));
            let seen: HashSet<u64> = (0..n).map(|i| p.apply(i)).collect();
            assert_eq!(seen.len() as u64, n);
            assert!(seen.iter().all(|&x| x < n));
        }
    }

    #[test]
    fn permutations_differ_by_key() {
        let a = Permutation::new(1000, &key(1, &["t"]));
        let b = Permutation::new(1000, &key(2, &["t"]));
        assert!((0..20).any(|i| a.apply(i) != b.apply(i)));
    }

    #[test]
    fn cells_are_a_function_of_their_coordinates() {
        let k = key(42, &["public", "users", "email"]);
        let first: u64 = cell(&k, 17, 0).random();
        let again: u64 = cell(&k, 17, 0).random();
        let next_row: u64 = cell(&k, 18, 0).random();
        let redrawn: u64 = cell(&k, 17, 1).random();
        assert_eq!(first, again);
        assert_ne!(first, next_row);
        assert_ne!(first, redrawn);
        assert_ne!(key(42, &["a.b", "c"]), key(42, &["a", "b.c"]));
    }

    #[test]
    fn skewed_ranks_favour_the_first() {
        let k = key(3, &["fk"]);
        let mut counts = [0u32; 10];
        for row in 0..10_000 {
            counts[skewed(&mut cell(&k, row, 0), 10) as usize] += 1;
        }
        assert!(counts[0] > counts[9] * 3, "{counts:?}");
        assert!(counts.iter().all(|&c| c > 0));
    }
}
