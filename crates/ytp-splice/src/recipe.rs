//! Splice recipes: how to build one word out of others, as saved from the
//! Python app's splice editor into the corpus's `splice_recipes` table
//! (`_load_user_recipes` in `app/phonemes.py`). Consulted before the
//! built-in ones.

use serde_json::Value;

/// A piece cut by hand in the editor: that clip, exactly those times.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cut {
    pub clip_id: i64,
    pub start: f64,
    pub end: f64,
}

/// One unit: the phonemes it covers and the words to take them from, in
/// order of preference.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub phones: Vec<String>,
    pub from: Vec<String>,
    pub cut: Option<Cut>,
}

fn strings(v: &Value) -> Option<Vec<String>> {
    v.as_array()?.iter().map(|x| x.as_str().map(str::to_string)).collect()
}

/// The recipe stored as JSON `[{"phones": [...], "from": [...], "clip_id"?,
/// "start"?, "end"?}, ...]`, or `None` when it cannot be read.
pub fn parse(json: &str) -> Option<Vec<Group>> {
    let v: Value = serde_json::from_str(json).ok()?;
    v.as_array()?
        .iter()
        .map(|g| {
            let cut = match (g.get("clip_id"), g.get("start"), g.get("end")) {
                (Some(c), Some(s), Some(e)) => match (c.as_i64(), s.as_f64(), e.as_f64()) {
                    (Some(clip_id), Some(start), Some(end)) if clip_id != 0 => {
                        Some(Cut { clip_id, start, end })
                    }
                    _ => None,
                },
                _ => None,
            };
            Some(Group {
                phones: strings(g.get("phones")?)?,
                from: match g.get("from") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(f) => strings(f)?,
                },
                cut,
            })
        })
        .collect()
}

/// The groups of a built-in recipe `(phonemes, preferred sources)`.
pub fn from_pairs(pairs: &[(Vec<String>, Vec<String>)]) -> Vec<Group> {
    pairs
        .iter()
        .map(|(p, f)| Group { phones: p.clone(), from: f.clone(), cut: None })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_python_editor_wrote() {
        let g = parse(
            r#"[{"phones":["K","AE"],"from":["cat"]},
                {"phones":["P"],"from":["top"],"clip_id":7,"start":20.25,"end":20.4}]"#,
        )
        .unwrap();
        assert_eq!(g[0].phones, ["K", "AE"]);
        assert_eq!(g[0].from, ["cat"]);
        assert_eq!(g[0].cut, None);
        assert_eq!(g[1].cut, Some(Cut { clip_id: 7, start: 20.25, end: 20.4 }));
    }

    #[test]
    fn a_cut_needs_all_three_and_junk_is_refused() {
        let g = parse(r#"[{"phones":["P"],"from":["top"],"clip_id":7,"start":1.0}]"#).unwrap();
        assert_eq!(g[0].cut, None);
        assert!(parse("not json").is_none());
        assert!(parse(r#"[{"from":["x"]}]"#).is_none());
        assert!(parse(r#"{"phones":[]}"#).is_none());
    }
}
