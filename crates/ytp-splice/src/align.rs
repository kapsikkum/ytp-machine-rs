//! Where inside a word clip its letters and phonemes fall, from the
//! alignments stored in the corpus (`phones` column, `clip_alignments`
//! table). The Python app aligns missing ones with a neural model; here a
//! clip with neither has no alignment and callers take their fallbacks.

use crate::model::Clip;
use crate::pool::Pool;

const DIGRAPHS: [&str; 30] = [
    "tch", "sch", "th", "sh", "ch", "ph", "wh", "ck", "ng", "qu", "gh", "oo", "ee", "ea", "ai",
    "ay", "oa", "ow", "ou", "oy", "oi", "au", "aw", "ew", "ie", "igh", "ar", "er", "ir", "or",
];

fn grapheme_groups(word: &str) -> Vec<usize> {
    let w = word.to_lowercase();
    let mut groups = Vec::new();
    let mut i = 0;
    while i < w.len() {
        let rest = &w[i..];
        let matched = DIGRAPHS
            .iter()
            .find(|dg| rest.starts_with(**dg))
            .map_or(1, |dg| dg.len());
        // `matched` is a whole number of characters for ASCII digraphs; a
        // lone non-ASCII character counts as one group.
        let step = if matched == 1 {
            rest.chars().next().map_or(1, char::len_utf8)
        } else {
            matched
        };
        groups.push(matched);
        i += step;
    }
    groups
}

fn char_times<'p>(pool: &'p Pool, clip: &Clip) -> Option<&'p Vec<(char, f64, f64)>> {
    pool.alignments.get(&clip.id?)
}

/// Letter times of the clip with the given id, relative to its start.
pub fn char_times_by_id(pool: &Pool, id: Option<i64>) -> Option<&Vec<(char, f64, f64)>> {
    pool.alignments.get(&id?)
}

/// Where the word's last character ends, relative to the clip start.
pub fn word_core_end(pool: &Pool, clip: &Clip) -> Option<f64> {
    char_times(pool, clip)?.last().map(|c| c.2)
}

/// Start (relative to the clip) of the part left after skipping `skip` phonemes.
pub fn cut_start_before_phonemes(pool: &Pool, clip: &Clip, skip: usize) -> Option<f64> {
    if let Some(pt) = clip.phones.as_ref().filter(|p| !p.is_empty()) {
        return Some(if skip == 0 { pt[0].1 } else { pt[skip.min(pt.len() - 1)].1 });
    }
    let ct = char_times(pool, clip).filter(|c| !c.is_empty())?;
    if skip == 0 {
        return Some(ct[0].1);
    }
    let groups = grapheme_groups(&clip.word);
    if skip >= groups.len() {
        return Some(ct[ct.len() - 1].1);
    }
    let char_idx: usize = groups[..skip].iter().sum();
    if char_idx >= ct.len() {
        return Some(ct[ct.len() - 1].1);
    }
    Some(ct[char_idx].1)
}

/// End (relative to the clip) of the first `k` phonemes.
pub fn cut_end_after_phonemes(pool: &Pool, clip: &Clip, k: usize, last_is_vowel: bool) -> Option<f64> {
    if let Some(pt) = clip.phones.as_ref().filter(|p| !p.is_empty()) {
        let mut end = if k > 0 { pt[k.min(pt.len()) - 1].2 } else { pt[0].1 };
        if !last_is_vowel && k > 0 {
            let start = pt[k.min(pt.len()) - 1].1;
            end = end.max(start + 0.055);
            if k < pt.len() {
                end = end.min(pt[k].2);
            }
        }
        return Some(end);
    }
    let ct = char_times(pool, clip).filter(|c| !c.is_empty())?;
    let groups = grapheme_groups(&clip.word);
    if k >= groups.len() {
        return Some(ct[ct.len() - 1].2);
    }
    let char_idx: usize = groups[..k].iter().sum();
    if char_idx >= ct.len() || char_idx == 0 {
        return Some(if char_idx >= ct.len() {
            ct[ct.len() - 1].2
        } else {
            ct[char_idx].1
        });
    }
    if last_is_vowel {
        let mut cut = ct[char_idx].1;
        let mut j = char_idx;
        while j < ct.len() && "aeiouy".contains(ct[j].0.to_ascii_lowercase()) {
            cut = ct[j].2;
            j += 1;
        }
        return Some(cut);
    }
    let (cstart, cend) = (ct[char_idx - 1].1, ct[char_idx - 1].2);
    let cut = (cend + 0.03).max(cstart + 0.055);
    Some(cut.min(ct[char_idx].1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups() {
        assert_eq!(grapheme_groups("shoot"), vec![2, 2, 1]);
        assert_eq!(grapheme_groups("night"), vec![1, 3, 1]);
    }
}
