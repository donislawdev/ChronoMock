//! What the random properties of this crate share: numbers drawn from their edges as well as from
//! anywhere, and the canary of the weekly long run.
//!
//! Compiled for tests only (`#[cfg(test)] mod search;` in main.rs). The properties stand beside the
//! code they search, each in a `mod properties`, so `cargo test properties` selects every one of
//! them - the weekly run in .github/workflows/search.yml relies on that name. chrono-core has a file
//! of the same name and the same kind: a crate cannot reach another crate's test code.

use proptest::prelude::*;

/// Any `u64`, with its edges drawn as often as the middle. A uniform draw lands among the last few
/// values of a 64-bit range about once in 2^60 draws, and the length that panicked the WebSocket
/// parser sat exactly there (S-4) - so the edges are asked for by name.
pub(crate) fn edge_u64() -> impl Strategy<Value = u64> {
    prop_oneof![
        4 => any::<u64>(),
        2 => 0u64..=1_000,
        1 => (u64::MAX - 16)..=u64::MAX,
    ]
}

/// `around` and the few values either side of it - a limit is wrong on its edge, not far from it.
pub(crate) fn near(around: u64, by: u64) -> impl Strategy<Value = u64> {
    around.saturating_sub(by)..=around.saturating_add(by)
}

mod properties {
    use proptest::prelude::*;

    proptest! {
        /// The canary of the weekly run: red when CHRONO_SEARCH_CANARY is set, and a property that
        /// holds in every other run. Each part runs it first and stops if it does NOT see it fail,
        /// so a part that then finds nothing has shown it would have said so.
        #[test]
        fn the_canary_fails_only_when_asked_to(drawn in any::<u64>()) {
            let asked = std::env::var_os("CHRONO_SEARCH_CANARY").is_some();
            prop_assert!(!asked, "the canary failed, as it was asked to (drew {})", drawn);
        }
    }
}
