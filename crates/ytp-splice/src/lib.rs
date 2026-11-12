//! Choosing and planning the clips that say a sentence: a port of
//! `resolve_text` / `timeline` from `app/generate.py` and what they use of
//! `app/prosody.py` and `app/phonemes.py`.
//!
//! Pure over an in-memory [`Pool`]; audio is read through [`AudioProbe`]
//! and rendering lives in `ytp-media`.

pub mod align;
pub mod audio;
pub mod model;
pub mod options;
pub mod phoneme_splice;
pub mod pool;
pub mod project;
pub mod prosody;
pub mod recipe;
pub mod resolve;
pub mod rng;
pub mod select;
pub mod stretch;
pub mod window;

pub use audio::{AudioCache, AudioProbe, NoAudio};
pub use model::{Clip, Report, Segment, TokenReport};
pub use options::{removed_options, GenerationOptions};
pub use pool::Pool;
pub use project::{project_from_plan, segment_of_clip, segments_from_track, Locator, PlanInfo};
pub use resolve::{resolve_text, Cancelled, Progress, Runtime};
pub use rng::PyRandom;
pub use window::{extract_window, segment_durations, timeline, total_duration, WordSpan};

/// The generator for `opts`: an empty seed picks a new one (and is filled in,
/// so the same options replayed give the same video).
pub fn seeded_rng(opts: &mut GenerationOptions) -> PyRandom {
    if opts.seed.is_empty() {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        opts.seed = (h.finish() % (1 << 30)).to_string();
    }
    PyRandom::from_str_seed(&opts.seed)
}
