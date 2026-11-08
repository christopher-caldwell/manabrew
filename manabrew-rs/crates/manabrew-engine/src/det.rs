//! Hash collections with a fixed hasher.
//!
//! `std`'s `HashMap` and `HashSet` start every instance from random keys, so iterating one gives a
//! different order in every run. Anything that walks such a collection (the cards in a target
//! set, the entries of a counter table) then acts in a different order from one run to the next,
//! and a game with a fixed seed stops being reproducible. These aliases use SipHash with fixed
//! keys: the order depends only on what was inserted and when, so the same game replays the
//! same way. Build them with `default()`, not `new()`.

use std::collections::hash_map::DefaultHasher;
use std::hash::BuildHasherDefault;

pub type DetState = BuildHasherDefault<DefaultHasher>;
pub type HashMap<K, V> = std::collections::HashMap<K, V, DetState>;
pub type HashSet<T> = std::collections::HashSet<T, DetState>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iteration_order_is_the_same_for_every_instance() {
        let build = || {
            let mut set = HashSet::default();
            for n in [17u32, 3, 99, 42, 8, 1, 64, 23] {
                set.insert(n);
            }
            set.into_iter().collect::<Vec<_>>()
        };
        let first = build();
        for _ in 0..20 {
            assert_eq!(build(), first);
        }
    }
}
