//! Pluggable RNG for game effects (shuffles, coin flips, dice rolls).
//!
//! By default, effects use the system's thread-local RNG. For parity testing,
//! a deterministic RNG (e.g. JavaRandom) can be injected to match Java Forge's
//! `MyRandom` consumption order exactly.
//!
//! # WASM Compatibility
//!
//! The `rand` crate 0.8+ supports WASM via `getrandom`. For browser WASM,
//! ensure the WASM entry point crate (wasm) includes:
//! ```toml
//! getrandom = { version = "0.2", features = ["js"] }
//! ```
//! This enables `thread_rng()` to work in browser environments.

use crate::ids::CardId;

/// Trait for game-level randomness, used by effect resolvers.
///
/// This abstraction lets parity tests inject a Java-compatible RNG
/// that matches `java.util.Random` and `Collections.shuffle()` exactly,
/// while normal gameplay uses the default thread-local RNG.
pub trait GameRng {
    /// Shuffle a slice of CardIds in-place.
    /// Must match `java.util.Collections.shuffle(list, rng)` for parity.
    fn shuffle_cards(&mut self, cards: &mut [CardId]);

    /// Return a random integer in `[0, bound)`.
    /// Must match `java.util.Random.nextInt(bound)` for parity.
    fn next_int(&mut self, bound: i32) -> i32;

    /// Debug: return the total number of RNG calls made so far (if tracked).
    fn call_count(&self) -> u64 {
        0
    }
}

thread_local! {
    static SEEDED: std::cell::RefCell<Option<rand::rngs::StdRng>> = const { std::cell::RefCell::new(None) };
}

/// Seeds this thread's effect RNG. Every effect that shuffles or rolls through
/// [`ThreadRngAdapter`], [`with_effect_rng`] or `CardLists::shuffle` then draws from one seeded
/// stream, so the same seed and the same decisions replay the same game. Until it is called,
/// the RNG is the system's thread-local one.
pub fn seed_effect_rng(seed: u64) {
    use rand::SeedableRng;
    SEEDED.with(|rng| *rng.borrow_mut() = Some(rand::rngs::StdRng::seed_from_u64(seed)));
}

/// Runs `f` with this thread's seeded effect RNG, or the system thread RNG if none was seeded.
pub fn with_effect_rng<T>(f: impl FnOnce(&mut dyn rand::RngCore) -> T) -> T {
    SEEDED.with(|rng| match rng.borrow_mut().as_mut() {
        Some(seeded) => f(seeded),
        None => f(&mut rand::thread_rng()),
    })
}

/// Default effect RNG: the seeded stream if [`seed_effect_rng`] was called on this thread,
/// otherwise `rand::thread_rng()` (non-deterministic).
pub struct ThreadRngAdapter;

impl GameRng for ThreadRngAdapter {
    fn shuffle_cards(&mut self, cards: &mut [CardId]) {
        use rand::seq::SliceRandom;
        with_effect_rng(|rng| cards.shuffle(rng));
    }

    fn next_int(&mut self, bound: i32) -> i32 {
        use rand::Rng;
        with_effect_rng(|rng| rng.gen_range(0..bound))
    }
}
