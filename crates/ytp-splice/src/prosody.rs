//! Choosing takes that sound alike (`app/prosody.py`, the generation side).

use std::collections::HashMap;

use crate::model::{Clip, Tone, F_BRIGHTNESS, F_LOUDNESS, F_PITCH_END, F_PITCH_START};

const SAME_CATEGORY: f64 = 100.0;
const SAME_SOURCE: f64 = 20.0;
const JOIN_WIDTH: f64 = 0.35;

/// The sentence being put together: which category to lean to, and the take
/// just used.
#[derive(Debug, Clone)]
pub struct ToneCtx {
    pub strength: f64,
    pub target: Option<i64>,
    pub prev: Option<(Tone, i64)>,
}

impl ToneCtx {
    pub fn new(strength: f64) -> Option<Self> {
        (strength > 0.0).then_some(Self {
            strength,
            target: None,
            prev: None,
        })
    }

    /// Record `tone`/`source_id` as the take the next pick should join on to.
    pub fn follow(&mut self, tone: Option<&Tone>, source_id: i64) {
        if let Some(t) = tone {
            self.prev = Some((t.clone(), source_id));
        }
    }

    /// How much likelier `clip` is to be picked, for its tone alone.
    pub fn weight(&self, clip: &Clip) -> f64 {
        let Some(tone) = &clip.tone else { return 1.0 };
        let s = self.strength;
        let mut w = 1.0;
        if self.target.is_some() && tone.cat == self.target {
            w *= SAME_CATEGORY.powf(s);
        }
        if let Some((prev, prev_src)) = &self.prev {
            if *prev_src == clip.source_id {
                w *= SAME_SOURCE.powf(s);
            }
            let (mut d2, mut used) = (0.0, 0);
            for (i, j) in [
                (F_PITCH_END, F_PITCH_START),
                (F_LOUDNESS, F_LOUDNESS),
                (F_BRIGHTNESS, F_BRIGHTNESS),
            ] {
                if let (Some(a), Some(b)) = (prev.z[i], tone.z[j]) {
                    d2 += (a - b) * (a - b);
                    used += 1;
                }
            }
            if used > 0 {
                w *= (-s * d2 / used as f64 / (2.0 * JOIN_WIDTH * JOIN_WIDTH)).exp();
            }
        }
        w
    }
}

/// The category the most of these words can be said in; ties go to the
/// category with more takes, then the lower number.
pub fn target(candidates: &[Vec<&Clip>]) -> Option<i64> {
    let mut cover: HashMap<i64, usize> = HashMap::new();
    let mut count: HashMap<i64, usize> = HashMap::new();
    for takes in candidates {
        let cats: Vec<i64> = takes.iter().filter_map(|c| c.tone.as_ref()?.cat).collect();
        let mut seen: Vec<i64> = cats.clone();
        seen.sort_unstable();
        seen.dedup();
        for c in seen {
            *cover.entry(c).or_default() += 1;
        }
        for c in cats {
            *count.entry(c).or_default() += 1;
        }
    }
    cover
        .keys()
        .copied()
        .max_by_key(|c| (cover[c], count[c], -c))
}
