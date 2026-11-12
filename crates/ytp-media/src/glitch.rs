//! The glitch vocabulary: name -> (video filters, audio filters).
//!
//! Two rules, both of which break the whole encode rather than one word:
//! every video entry must end back at 480x270 (concat demands identical size
//! and SAR from every segment in a chunk), and no commas inside an expression
//! (the chain is comma-joined).

pub struct Glitch {
    pub name: &'static str,
    pub video: &'static [&'static str],
    pub audio: &'static [&'static str],
}

pub const GLITCHES: &[Glitch] = &[
    Glitch {
        name: "shake",
        video: &[
            "crop=w=440:h=250:x='20+18*sin(t*31)':y='10+12*sin(t*43)'",
            "scale=480:270",
        ],
        audio: &[],
    },
    Glitch { name: "punch", video: &["scale=600:338", "crop=480:270"], audio: &[] },
    Glitch { name: "spin", video: &["rotate='0.5*sin(t*9)':c=black"], audio: &[] },
    Glitch { name: "rainbow", video: &["hue=h='t*900':s=2"], audio: &[] },
    Glitch { name: "invert", video: &["negate"], audio: &[] },
    Glitch { name: "flip", video: &["hflip"], audio: &[] },
    Glitch {
        name: "strobe",
        video: &["eq=brightness='0.45*sin(t*38)':contrast=1.6:eval=frame"],
        audio: &[],
    },
    Glitch {
        name: "crunch",
        video: &["scale=64:36:flags=neighbor", "scale=480:270:flags=neighbor"],
        audio: &[],
    },
    Glitch { name: "fringe", video: &["rgbashift=rh=7:bh=-7"], audio: &[] },
    Glitch {
        name: "earrape",
        video: &[],
        audio: &["volume=17dB", "acrusher=bits=5:mode=lin", "alimiter=limit=0.95"],
    },
    // Not vibrato: it hands the encoder NaN on some runs and not others.
    Glitch { name: "wobble", video: &[], audio: &["flanger=delay=3:depth=9:speed=6:width=100"] },
    Glitch { name: "cave", video: &[], audio: &["aecho=0.8:0.85:55:0.45"] },
    Glitch {
        name: "stutter",
        video: &["loop=loop=3:size=4:start=0"],
        audio: &["aloop=loop=3:size=3600:start=0"],
    },
];

pub fn find(name: &str) -> Option<&'static Glitch> {
    GLITCHES.iter().find(|g| g.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_text_crate_vocabulary() {
        let names: Vec<&str> = GLITCHES.iter().map(|g| g.name).collect();
        assert_eq!(names, ytp_text::tokenize::GLITCH_NAMES);
    }

    #[test]
    fn no_filter_contains_a_bare_comma() {
        for g in GLITCHES {
            for f in g.video.iter().chain(g.audio) {
                let mut quoted = false;
                for c in f.chars() {
                    if c == '\'' {
                        quoted = !quoted;
                    }
                    assert!(quoted || c != ',', "{}: {f}", g.name);
                }
            }
        }
    }
}
