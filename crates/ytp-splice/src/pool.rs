//! The lookup structures one corpus is read into (`_pool` in
//! `app/generate.py`): clips by word, in spoken order per source, word
//! positions for phrase runs, source quality, idle footage, noises, votes.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use ytp_corpus::data::{resolve_in, ClipRow, CorpusData, TONE_FEATURES};

use crate::model::{Clip, Tone};
use crate::recipe;

pub const MIN_DUR: f64 = 0.05;
pub const IDLE_MIN_GAP: f64 = 0.45;

#[derive(Debug, Clone)]
pub struct IdleRegion {
    pub source_file: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Default)]
pub struct Pool {
    pub slug: String,
    pub clips: Vec<Clip>,
    /// Word -> clips usable on their own (>= `MIN_DUR`), in corpus order.
    pub by_word: IndexMap<String, Vec<usize>>,
    /// Source id -> clips in spoken order.
    pub ordered_by_source: IndexMap<i64, Vec<usize>>,
    /// Word -> `(source id, index in that source's order)`.
    pub word_positions: HashMap<String, Vec<(i64, usize)>>,
    pub source_quality: HashMap<i64, f64>,
    pub idle: Vec<IdleRegion>,
    pub noises: Vec<usize>,
    pub noise_by_word: IndexMap<String, Vec<usize>>,
    /// `(word, clip id)` -> net votes.
    pub splice_scores: HashMap<String, HashMap<i64, i64>>,
    pub boundary_reports: HashMap<i64, i64>,
    pub titles: HashMap<i64, String>,
    pub settings: HashMap<String, String>,
    pub alignments: HashMap<i64, Vec<(char, f64, f64)>>,
    /// This corpus's pronunciations.csv (and the shared one), if any rows.
    pub pron: Option<Arc<ytp_text::UserDict>>,
    /// Saved splice recipes by word (splice_recipes).
    pub recipes: HashMap<String, Vec<recipe::Group>>,
}

fn parse_phones(raw: &Option<String>) -> Option<Vec<(String, f64, f64)>> {
    let v: Value = serde_json::from_str(raw.as_deref()?).ok()?;
    let out: Option<Vec<_>> = v
        .as_array()?
        .iter()
        .map(|p| {
            Some((
                p.get(0)?.as_str()?.to_string(),
                p.get(1)?.as_f64()?,
                p.get(2)?.as_f64()?,
            ))
        })
        .collect();
    out.filter(|o| !o.is_empty())
}

fn scaled(feature: usize, v: f64) -> f64 {
    if TONE_FEATURES[feature] == "pace" {
        v.max(1e-3).ln()
    } else {
        v
    }
}

fn load_tone(data: &CorpusData) -> HashMap<i64, Tone> {
    let stats: Option<Value> = data
        .settings
        .get("tone_stats")
        .and_then(|s| serde_json::from_str(s).ok());
    let (Some(stats), false) = (stats, data.tone.is_empty()) else {
        return HashMap::new();
    };
    data.tone
        .iter()
        .map(|row| {
            let mut z = [None; 7];
            for (i, f) in TONE_FEATURES.iter().enumerate() {
                let (mean, sd) = (
                    stats[f][0].as_f64().unwrap_or(0.0),
                    stats[f][1].as_f64().unwrap_or(1.0),
                );
                z[i] = row.features[i].map(|v| ((scaled(i, v) - mean) / sd).clamp(-3.0, 3.0));
            }
            (row.clip_id, Tone { z, cat: row.category })
        })
        .collect()
}

impl Pool {
    /// Phoneme lookups on this thread use this corpus's pronunciations until
    /// the guard drops.
    pub fn scope(&self) -> ytp_text::userdict::Scope {
        ytp_text::userdict::scope(self.pron.as_ref())
    }

    pub fn splice_mode(&self) -> &str {
        match self.settings.get("splice_mode").map(|s| s.to_lowercase()) {
            Some(m) if m == "loose" => "loose",
            Some(m) if m == "desperate" => "desperate",
            _ => "strict",
        }
    }

    pub fn max_units(&self) -> usize {
        self.settings
            .get("max_units")
            .filter(|s| !s.is_empty())
            .and_then(|s| s.trim().parse::<i64>().ok())
            .map_or(8, |n| n.clamp(2, 24) as usize)
    }

    /// Build the pool the way `_pool` does, from a corpus read into memory.
    pub fn build(data: &CorpusData) -> Self {
        let mut pool = Pool {
            slug: data.name.clone(),
            titles: data.titles.clone(),
            settings: data.settings.clone(),
            splice_scores: {
                let mut m: HashMap<String, HashMap<i64, i64>> = HashMap::new();
                for (w, c, s) in &data.ratings {
                    m.entry(w.clone()).or_default().insert(*c, *s);
                }
                m
            },
            boundary_reports: data.boundary_reports.iter().copied().collect(),
            ..Default::default()
        };
        if data.pronunciations.is_some() || data.shared_pronunciations.is_some() {
            let dict = ytp_text::UserDict::from_csv(
                data.shared_pronunciations.as_deref(),
                data.pronunciations.as_deref(),
            );
            for (origin, word, value) in &dict.rejected {
                tracing::warn!(corpus = %data.name, ?origin, %word, %value, "pronunciations.csv: could not make sense of the value (use ARPAbet, words that sound right, =letters or =skip)");
            }
            pool.pron = (!dict.is_empty()).then(|| Arc::new(dict));
        }
        let _pronunciations = pool.scope();
        for (word, json) in &data.recipes {
            match recipe::parse(json) {
                Some(groups) => {
                    pool.recipes.insert(word.clone(), groups);
                }
                None => tracing::warn!(corpus = %data.name, %word, "unreadable splice recipe"),
            }
        }
        for (id, raw) in &data.alignments {
            if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(raw) {
                let al: Vec<(char, f64, f64)> = a
                    .iter()
                    .filter_map(|e| {
                        Some((
                            e.get(0)?.as_str()?.chars().next()?,
                            e.get(1)?.as_f64()?,
                            e.get(2)?.as_f64()?,
                        ))
                    })
                    .collect();
                if !al.is_empty() {
                    pool.alignments.insert(*id, al);
                }
            }
        }
        let tone = load_tone(data);
        let mk = |r: &ClipRow| -> Option<Clip> {
            let source_file = resolve_in(&data.dir, &r.source_file)?;
            Some(Clip {
                id: Some(r.id),
                source_id: r.source_id,
                word: r.word.clone(),
                start_time: r.start_time,
                end_time: r.end_time,
                source_file: source_file.to_string_lossy().into_owned(),
                prev_end: None,
                next_start: None,
                bleed_risk: false,
                edited: r.edited,
                tone: tone.get(&r.id).cloned(),
                phones: parse_phones(&r.phones),
            })
        };

        let mut src_total: HashMap<i64, usize> = HashMap::new();
        let mut src_good: HashMap<i64, usize> = HashMap::new();
        for r in &data.clips {
            let Some(clip) = mk(r) else {
                tracing::warn!(corpus = %data.name, path = %r.source_file, "clip outside the corpus ignored");
                continue;
            };
            *src_total.entry(clip.source_id).or_default() += 1;
            if clip.duration() >= MIN_DUR {
                *src_good.entry(clip.source_id).or_default() += 1;
            }
            if clip.duration() > 0.0 {
                let sid = clip.source_id;
                pool.clips.push(clip);
                pool.ordered_by_source
                    .entry(sid)
                    .or_default()
                    .push(pool.clips.len() - 1);
            }
        }
        pool.link_neighbours();
        pool.source_quality = src_total
            .iter()
            .map(|(s, t)| (*s, *src_good.get(s).unwrap_or(&0) as f64 / *t as f64))
            .collect();

        for r in &data.noises {
            let Some(mut c) = mk(r) else { continue };
            c.prev_end = Some(c.start_time);
            c.next_start = Some(c.end_time);
            c.tone = None;
            c.phones = None;
            let word = c.word.clone();
            pool.clips.push(c);
            let idx = pool.clips.len() - 1;
            pool.noises.push(idx);
            pool.noise_by_word.entry(word).or_default().push(idx);
        }
        pool
    }

    /// Neighbour times, bleed risk, word positions, usable-by-word index and
    /// idle regions, from `ordered_by_source`.
    fn link_neighbours(&mut self) {
        let sources: Vec<(i64, Vec<usize>)> = self
            .ordered_by_source
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        for (src, seq) in sources {
            for (idx, &ci) in seq.iter().enumerate() {
                let prev_end = (idx > 0).then(|| self.clips[seq[idx - 1]].end_time);
                let next = seq.get(idx + 1).map(|&n| (self.clips[n].start_time, self.clips[n].word.clone()));
                let mut bleed = false;
                if let Some((ns, nw)) = &next {
                    let gap = ns - self.clips[ci].end_time;
                    if gap < 0.12 {
                        if let Some(nph) = ytp_text::word_to_phonemes(nw) {
                            bleed = nph.first().is_some_and(|p| ytp_text::is_vowel(p));
                        }
                    }
                }
                let c = &mut self.clips[ci];
                c.prev_end = prev_end;
                c.next_start = next.as_ref().map(|n| n.0);
                c.bleed_risk = bleed;
                let word = c.word.clone();
                let (end, dur) = (c.end_time, c.duration());
                let source_file = c.source_file.clone();
                self.word_positions.entry(word.clone()).or_default().push((src, idx));
                if dur >= MIN_DUR {
                    self.by_word.entry(word).or_default().push(ci);
                }
                if let Some((ns, _)) = next {
                    if ns - end >= IDLE_MIN_GAP {
                        self.idle.push(IdleRegion {
                            source_file,
                            start: end + 0.06,
                            end: ns - 0.06,
                        });
                    }
                }
            }
        }
    }

    /// A pool from clips already carrying their neighbours (tests, tools).
    pub fn from_words(slug: &str, settings: &[(&str, &str)], words: Vec<(String, Vec<Clip>)>) -> Self {
        let mut pool = Pool {
            slug: slug.into(),
            settings: settings.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..Default::default()
        };
        for (w, clips) in words {
            let mut idxs = Vec::new();
            for c in clips {
                pool.clips.push(c);
                idxs.push(pool.clips.len() - 1);
            }
            pool.by_word.insert(w, idxs);
        }
        pool
    }

    pub fn clip(&self, i: usize) -> &Clip {
        &self.clips[i]
    }

    pub fn word_clips(&self, word: &str) -> Vec<&Clip> {
        self.by_word
            .get(word)
            .map(|v| v.iter().map(|&i| &self.clips[i]).collect())
            .unwrap_or_default()
    }

    pub fn clip_count(&self, word: &str) -> usize {
        self.by_word.get(word).map_or(0, Vec::len)
    }

    /// Net votes for `id` as a take of `word`.
    pub fn vote(&self, word: &str, id: i64) -> i64 {
        let get = |w: &str| {
            self.splice_scores
                .get(w)
                .and_then(|m| m.get(&id))
                .copied()
                .unwrap_or(0)
        };
        if word.chars().any(char::is_uppercase) {
            get(&word.to_lowercase())
        } else {
            get(word)
        }
    }

    /// `clip id -> cost adjustment` for splicing `word` (downvotes dear,
    /// upvotes cheap); unvoted clips are omitted.
    pub fn penalties(&self, word: &str) -> HashMap<i64, i64> {
        self.splice_scores
            .get(&word.to_lowercase())
            .map(|m| m.iter().filter(|(_, s)| **s != 0).map(|(c, s)| (*c, -*s)).collect())
            .unwrap_or_default()
    }
}

impl ytp_text::Lexicon for Pool {
    fn clip_count(&self, word: &str) -> usize {
        Pool::clip_count(self, word)
    }
}
