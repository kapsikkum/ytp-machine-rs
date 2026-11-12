//! What a chat message is asking for.
//!
//! A faithful port of the old Python bot's `app/matrix_commands.py`, kept
//! apart from any chat network so it can be tested without a homeserver:
//!
//! ```text
//! !ytp say nice chocolate cake            a sentence video, words on the picture
//! !ytp say --no-subs nice chocolate cake  ... without them
//! !ytp mv                                 the last MIDI posted here, as a YTPMV
//! !ytp mv lead=yeah kick=boom max=60      ... with some parts' sounds chosen
//! !ytp mv octave:bass=+1 mode:lead=tape
//! !ytp mv info                            what the song is and what it would use
//! !ytp voices / !ytp voice <name>         list or switch the corpus (admins)
//! !ytp queue                              how busy it is
//! !ytp help
//! ```
//!
//! In a direct message the prefix is optional, and a message that is not a
//! command at all is taken as something to say.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

pub const DEFAULT_PREFIX: &str = "!ytp";

/// Per-part settings the mv command accepts.
const PART_KEYS: &[&str] = &[
    "mode",
    "octave",
    "pan",
    "program",
    "sustain",
    "take",
    "tone",
    "transpose",
    "volume",
];
const MODES: &[&str] = &["perfect", "tape", "raw"];
const SUSTAINS: &[&str] = &["note", "ring"];
const TONES: &[&str] = &[
    "clean", "voice", "dac", "dpcm",
    // the Mega Drive's
    "bass", "lead", "organ", "brass", "bell", "piano", "strings", "dist-guitar", "clean-guitar",
    "synth-bass", "square-lead", "reed", "flute", "marimba", "choir", "stab", "md-kick", "md-snare",
    "md-hat", "md-tom", "md-cymbal", "md-perc",
    // the NES's
    "pulse", "pulse-thin", "pulse-full", "triangle", "nes-kick", "nes-snare", "nes-hat", "nes-tom",
    "nes-cymbal", "nes-perc",
    // the Master System's and the SNES's
    "psg", "psg-soft", "psg-bass", "psg-pluck", "psg-pcm", "brr", "sms-kick", "sms-snare", "sms-hat",
    "sms-tom", "sms-cymbal", "sms-perc",
    // the Game Boy's
    "gb-pulse", "gb-pulse-thin", "gb-pulse-full", "gb-pluck", "gb-wave", "gb-pcm", "gb-kick",
    "gb-snare", "gb-hat", "gb-tom", "gb-cymbal", "gb-perc",
    // the PC's and the C64's
    "opl2", "opl3", "snes", "sb-pcm", "sb16-pcm", "sid-digi", "sid-lead", "sid-bass", "sid-pad",
    "sid-rhythm", "sid-bell", "sid-kick", "sid-snare", "sid-hat", "sid-tom", "sid-cymbal", "sid-perc",
    "megadrive", "slap",
];
const CHIPS: &[&str] = &[
    "off",
    "no",
    "md",
    "md-voice",
    "nes",
    "nes-voice",
    "sms",
    "sms-voice",
    "snes",
    "snes-voice",
    "gb",
    "gb-voice",
    "gbc",
    "gbc-voice",
    "opl2",
    "opl2-voice",
    "opl3",
    "opl3-voice",
    "sid",
    "sid-voice",
    "sid8580",
    "sid8580-voice",
    "megadrive",
    "megadrive-voice",
];
const VARY: &[&str] = &["off", "rotate", "random", "ultra"];
const CHIP_ERROR: &str = "chip is one of off, md, md-voice, nes, nes-voice, \
sms, sms-voice, snes, snes-voice, gb, gb-voice, gbc, gbc-voice, \
opl2, opl2-voice, opl3, opl3-voice, \
sid, sid-voice, sid8580, sid8580-voice";

pub const HELP: &str = "\
{p} say <words>        — a video of the voice saying it (--no-subs to drop the captions)
{p} mv                 — play the last MIDI file posted here, one tile per instrument
{p} mv info            — what the song is, and which word each part would use
{p} mv chip=md         — the whole song on an emulated sound chip, every part of it.
{p} mv chip=nes          md is four-operator FM, nes is two pulses and a noise
{p} mv chip=sms          register, sms is three squares and nothing else. All three
{p} mv chip=snes         throw the voice away -- add -voice to keep it instead,
{p} mv chip=gb           (gb and gbc share a chip and differ in their screens)
{p} mv chip=opl2         Doom's instrument bank on an AdLib; opl3 on a Sound Blaster
{p} mv chip=sid          the C64: sid is the 6581, sid8580 the later chip
                         played off that machine's own sample channel. snes plays
                         a General MIDI bank cut down to what a SNES could carry.
                         The picture goes through the same console's resolution
                         and colours
{p} mv lead=yeah kick=boom max=60 octave:bass=+1 tone:bass=bass
                         vary=rotate|random (several takes of a part's word rather
                         than one) or vary=ultra (a different word every hit, still
                         on the note), jitter=on (a hair of level and tuning per hit),
                         choose a part's word, or tweak it; parts are named by role
                         (lead, bass, rhythm, chords, kick, snare, hats, toms, cymbals)
                         or by instrument; tone:<part>= picks what one part is played
                         by, whatever the chip (sid-bell, pulse, triangle, dac...;
                         voice leaves the part as recorded); program:<part>= swaps its
                         General MIDI instrument, which picks the OPL patch
                         (program:lead=29 is overdriven guitar); a sound can be a word, a noise (*spew*)
                         or one phoneme cut out of words (/ah/, /s/); max= caps the
                         length in seconds
{p} mv balance=off     — leave the parts at their table levels instead of measuring
                         each one and moving it to where its job wants it
{p} voices             — the voices available
{p} voice <name>       — switch voice (admins only)
{p} queue              — how many videos are waiting
Reply to a MIDI file with {p} mv to use that one instead of the latest.";

pub fn help_text(prefix: &str) -> String {
    HELP.replace("{p}", prefix)
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandName {
    Say,
    Mv,
    Info,
    Voices,
    Voice,
    Queue,
    #[default]
    Help,
    Unknown,
}

impl CommandName {
    pub fn as_str(self) -> &'static str {
        match self {
            CommandName::Say => "say",
            CommandName::Mv => "mv",
            CommandName::Info => "info",
            CommandName::Voices => "voices",
            CommandName::Voice => "voice",
            CommandName::Queue => "queue",
            CommandName::Help => "help",
            CommandName::Unknown => "unknown",
        }
    }
}

/// What the message asked for, or the mistakes it made.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Command {
    pub name: CommandName,
    /// say: the words; voice: the slug; unknown: what was typed.
    pub text: String,
    /// Captions are the default; `--no-subs` turns them off.
    pub subtitles: bool,
    /// mv: part name → settings (`{"text": ...}`, `{"octave": 1}`, ...).
    pub parts: BTreeMap<String, Map<String, Value>>,
    /// mv: song options (`{"max_seconds": 60.0, ...}`).
    pub options: Map<String, Value>,
    pub errors: Vec<String>,
}

impl Command {
    fn new(name: CommandName) -> Self {
        Command {
            name,
            subtitles: true,
            ..Default::default()
        }
    }
}

/// A part of a song, as the analysis describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub instrument: String,
}

fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn bool_of(v: &str) -> Option<bool> {
    match v.to_lowercase().as_str() {
        "1" | "yes" | "on" | "true" | "y" => Some(true),
        "0" | "no" | "off" | "false" | "n" => Some(false),
        _ => None,
    }
}

/// Python's shlex.split, falling back to a plain split on an unbalanced quote.
fn tokens(s: &str) -> Vec<String> {
    shlex::split(s).unwrap_or_else(|| s.split_whitespace().map(str::to_string).collect())
}

fn is_str_set(list: &[&str], v: &str) -> bool {
    list.contains(&v)
}

/// The Python bot joined sorted sets when it listed the alternatives.
fn sorted_list(list: &[&str]) -> String {
    let mut v: Vec<&str> = list.to_vec();
    v.sort_unstable();
    v.join(", ")
}

fn part_conv(setting: &str) -> Option<&'static str> {
    match setting {
        "octave" | "take" | "transpose" | "program" => Some("int"),
        "volume" | "pan" => Some("float"),
        "mode" => Some("str"),
        "sustain" => Some("str"),
        "tone" => Some("str"),
        _ => None,
    }
}

fn strip_plus(v: &str) -> &str {
    v.trim_start_matches('+')
}

fn mv(args: Vec<String>) -> Command {
    let mut cmd = Command::new(CommandName::Mv);
    if let Some(first) = args.first() {
        if matches!(first.to_lowercase().as_str(), "info" | "describe" | "what") {
            return Command::new(CommandName::Info);
        }
    }
    for tok in args {
        let Some((key, value)) = tok.split_once('=') else {
            cmd.errors.push(format!(
                "don't know what to do with {} — expected part=word or key=value",
                py_repr(&tok)
            ));
            continue;
        };
        let key = key.trim().to_lowercase();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            cmd.errors
                .push(format!("{} is missing a side", py_repr(&tok)));
            continue;
        }
        if let Some((setting, part)) = key.split_once(':') {
            let setting = setting.to_string();
            let part = part.to_string();
            let Some(conv) = part_conv(&setting) else {
                cmd.errors.push(format!(
                    "unknown part setting {} (try {})",
                    py_repr(&setting),
                    PART_KEYS.join(", ")
                ));
                continue;
            };
            if part.is_empty() {
                cmd.errors.push(format!(
                    "unknown part setting {} (try {})",
                    py_repr(&setting),
                    PART_KEYS.join(", ")
                ));
                continue;
            }
            let parsed: Result<Value, ()> = match conv {
                "int" => strip_plus(value)
                    .parse::<i64>()
                    .map(Value::from)
                    .map_err(|_| ()),
                "float" => strip_plus(value)
                    .parse::<f64>()
                    .map(|v| json!(v))
                    .map_err(|_| ()),
                _ => Ok(Value::String(value.to_lowercase())),
            };
            let parsed = match parsed {
                Ok(v) => v,
                Err(_) => {
                    cmd.errors.push(format!(
                        "{setting} for {part} should be a number, not {}",
                        py_repr(value)
                    ));
                    continue;
                }
            };
            if setting == "mode" {
                if let Some(v) = parsed.as_str() {
                    if !is_str_set(MODES, v) {
                        cmd.errors
                            .push(format!("mode is one of {}", sorted_list(MODES)));
                        continue;
                    }
                }
            }
            if setting == "sustain" {
                if let Some(v) = parsed.as_str() {
                    if !is_str_set(SUSTAINS, v) {
                        cmd.errors
                            .push(format!("sustain is one of {}", sorted_list(SUSTAINS)));
                        continue;
                    }
                }
            }
            if setting == "tone" {
                if let Some(v) = parsed.as_str() {
                    if !is_str_set(TONES, v) {
                        cmd.errors.push(format!("tone is one of {}", sorted_list(TONES)));
                        continue;
                    }
                }
            }
            if setting == "program" {
                if let Some(v) = parsed.as_i64() {
                    if !(0..=127).contains(&v) {
                        cmd.errors
                            .push("program is a General MIDI instrument, 0 to 127".into());
                        continue;
                    }
                }
            }
            cmd.parts.entry(part).or_default().insert(setting, parsed);
        } else if let Some(kind) = option_kind(&key) {
            let name = option_name(&key);
            match kind {
                "chip" => {
                    let v = value.to_lowercase();
                    if !is_str_set(CHIPS, &v) {
                        cmd.errors.push(CHIP_ERROR.into());
                        continue;
                    }
                    let v = if v == "off" || v == "no" {
                        Value::Bool(false)
                    } else {
                        Value::String(v)
                    };
                    cmd.options.insert(name.to_string(), v);
                }
                "vary" => {
                    let v = value.to_lowercase();
                    if !is_str_set(VARY, &v) {
                        cmd.errors
                            .push(format!("vary is one of {}", sorted_list(VARY)));
                        continue;
                    }
                    cmd.options.insert(name.to_string(), Value::String(v));
                }
                "bool" => {
                    let Some(b) = bool_of(value) else {
                        cmd.errors.push(format!("{key} should be on or off"));
                        continue;
                    };
                    cmd.options.insert(name.to_string(), Value::Bool(b));
                }
                "float" => match strip_plus(value).parse::<f64>() {
                    Ok(v) => {
                        cmd.options.insert(name.to_string(), json!(v));
                    }
                    Err(_) => cmd.errors.push(format!(
                        "{key} should be a number, not {}",
                        py_repr(value)
                    )),
                },
                "int" => match strip_plus(value).parse::<i64>() {
                    Ok(v) => {
                        cmd.options.insert(name.to_string(), Value::from(v));
                    }
                    Err(_) => cmd.errors.push(format!(
                        "{key} should be a number, not {}",
                        py_repr(value)
                    )),
                },
                _ => unreachable!(),
            }
        } else {
            // part=word(s)
            let lower = value.to_lowercase();
            let entry = cmd.parts.entry(key).or_default();
            if matches!(lower.as_str(), "mute" | "off" | "none" | "-") {
                entry.insert("mute".into(), Value::Bool(true));
                entry.insert("visible".into(), Value::Bool(false));
            } else {
                entry.insert("text".into(), Value::String(value.replace('_', " ")));
            }
        }
    }
    cmd
}

fn option_kind(key: &str) -> Option<&'static str> {
    match key {
        "max" | "speed" => Some("float"),
        "transpose" | "key" => Some("int"),
        "vary" => Some("vary"),
        "jitter" | "flip" | "flash" | "dim" | "labels" | "balance" => Some("bool"),
        "chip" => Some("chip"),
        _ => None,
    }
}

fn option_name(key: &str) -> &'static str {
    match key {
        "max" => "max_seconds",
        "vary" => "vary",
        "jitter" => "jitter",
        "speed" => "speed",
        "transpose" | "key" => "transpose",
        "flip" => "flip",
        "flash" => "flash",
        "dim" => "dim",
        "labels" => "labels",
        "chip" => "chip",
        "balance" => "balance",
        _ => "",
    }
}

/// The first word of a string, by Python's `\w` notion (letters, digits, `_`).
fn first_word(s: &str) -> &str {
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..end]
}

/// The command in `body`, or `None` when the message is not for the bot.
///
/// `direct` is a one-to-one room: the prefix may be left off, and plain text
/// means "say this".
pub fn parse(body: &str, prefix: &str, direct: bool) -> Option<Command> {
    let text = body.trim();
    if text.is_empty() {
        return None;
    }
    // A reply quotes the message it answers in "> " lines ahead of the reply
    // itself; those are not what was asked.
    let text = text
        .lines()
        .filter(|ln| !ln.starts_with('>'))
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let low = text.to_lowercase();
    let p = prefix.to_lowercase();
    let rest = if low == p || low.starts_with(&(p.clone() + " ")) || low.starts_with(&(p.clone() + "\n"))
    {
        text[prefix.len()..].trim()
    } else if direct {
        let first = first_word(text).to_lowercase();
        let is_verb = matches!(
            first.as_str(),
            "say" | "mv" | "ytpmv" | "voice" | "voices" | "queue" | "help"
        );
        if !is_verb {
            return Some(Command {
                name: CommandName::Say,
                text: text.to_string(),
                subtitles: true,
                ..Default::default()
            });
        }
        text
    } else {
        return None;
    };

    if rest.is_empty() {
        return Some(Command::new(CommandName::Help));
    }
    let (verb, args) = match rest.split_once(' ') {
        Some((v, a)) => (v, a.trim()),
        None => (rest, ""),
    };
    let verb = verb.to_lowercase();
    let verb = verb.trim();
    match verb {
        "say" | "s" => {
            let mut subs = true;
            let mut words = args.to_string();
            for (flag, on) in [
                ("--subs", true),
                ("--subtitles", true),
                ("-s", true),
                ("--no-subs", false),
                ("--nosubs", false),
                ("--no-subtitles", false),
            ] {
                if words.starts_with(&(flag.to_string() + " ")) || words == flag {
                    subs = on;
                    words = words[flag.len()..].trim().to_string();
                    break;
                }
            }
            if words.is_empty() {
                let mut c = Command::new(CommandName::Help);
                c.errors.push("say what?".into());
                return Some(c);
            }
            Some(Command {
                name: CommandName::Say,
                text: words,
                subtitles: subs,
                ..Default::default()
            })
        }
        "mv" | "ytpmv" | "midi" | "song" => Some(mv(tokens(args))),
        "voices" => Some(Command::new(CommandName::Voices)),
        "voice" => Some(if args.is_empty() {
            Command::new(CommandName::Voices)
        } else {
            Command {
                name: CommandName::Voice,
                text: args.to_string(),
                ..Default::default()
            }
        }),
        "queue" | "status" => Some(Command::new(CommandName::Queue)),
        "help" | "?" => Some(Command::new(CommandName::Help)),
        _ => Some(Command {
            name: CommandName::Unknown,
            text: verb.to_string(),
            ..Default::default()
        }),
    }
}

/// Is this attachment a MIDI file (by name or type)?
pub fn is_midi(filename: Option<&str>, mimetype: Option<&str>) -> bool {
    let name = filename.unwrap_or("").to_lowercase();
    name.ends_with(".mid")
        || name.ends_with(".midi")
        || name.ends_with(".kar")
        || name.ends_with(".smf")
        || matches!(
            mimetype.unwrap_or("").to_lowercase().as_str(),
            "audio/midi" | "audio/x-midi" | "audio/mid"
        )
}

fn alias_role(n: &str) -> Option<&'static str> {
    Some(match n {
        "melody" | "tune" => "lead",
        "drums" => "kick",
        "hat" | "hihat" | "hihats" => "hats",
        "cymbal" => "cymbals",
        "tom" => "toms",
        "backing" | "guitar" => "rhythm",
        "pad" => "chords",
        _ => return None,
    })
}

/// Which part of a song `name` means: an id, a role, or a word of its name.
pub fn match_part<'a>(name: &str, parts: &'a [PartInfo]) -> Option<&'a PartInfo> {
    let n = name.trim().to_lowercase();
    if n.is_empty() {
        return None;
    }
    if let Some(p) = parts.iter().find(|p| p.id.to_lowercase() == n) {
        return Some(p);
    }
    for p in parts {
        let role = p.role.to_lowercase();
        if role == n || role.rsplit(':').next() == Some(n.as_str()) {
            return Some(p);
        }
    }
    if let Some(alias) = alias_role(&n) {
        return match_part(alias, parts);
    }
    parts
        .iter()
        .find(|p| p.instrument.to_lowercase().contains(&n) || p.name.to_lowercase().contains(&n))
}

/// The list of tones and chips, for docs and tests.
pub fn known_tones() -> BTreeSet<&'static str> {
    TONES.iter().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(body: &str) -> Option<Command> {
        parse(body, DEFAULT_PREFIX, false)
    }

    #[test]
    fn addressing() {
        assert!(p("nice chocolate cake").is_none());
        assert_eq!(p("!ytp").unwrap().name, CommandName::Help);
        assert_eq!(p("!YTP say hi").unwrap().text, "hi");
        assert!(p("!ytpfoo say hi").is_none());
        let d = parse("nice cake", DEFAULT_PREFIX, true).unwrap();
        assert_eq!((d.name, d.text.as_str()), (CommandName::Say, "nice cake"));
        assert_eq!(
            parse("mv lead=yeah", DEFAULT_PREFIX, true).unwrap().name,
            CommandName::Mv
        );
        assert_eq!(
            parse("!rosen say hi", "!rosen", false).unwrap().text,
            "hi"
        );
        assert_eq!(
            parse("> <@a:b> something\n!ytp say hello", DEFAULT_PREFIX, false)
                .unwrap()
                .text,
            "hello"
        );
    }

    #[test]
    fn say() {
        let c = p("!ytp say nice chocolate cake").unwrap();
        assert_eq!(
            (c.name, c.text.as_str(), c.subtitles),
            (CommandName::Say, "nice chocolate cake", true)
        );
        let c = p("!ytp say --subs nice cake").unwrap();
        assert_eq!((c.text.as_str(), c.subtitles), ("nice cake", true));
        let c = p("!ytp say --no-subs nice cake").unwrap();
        assert_eq!((c.text.as_str(), c.subtitles), ("nice cake", false));
        assert_eq!(p("!ytp say --no-subs").unwrap().name, CommandName::Help);
        assert_eq!(p("!ytp say").unwrap().name, CommandName::Help);
        assert_eq!(p("!ytp say ~nice~ *spew*").unwrap().text, "~nice~ *spew*");
    }

    #[test]
    fn mv() {
        let c = p("!ytp mv").unwrap();
        assert_eq!((c.name, c.parts.clone()), (CommandName::Mv, BTreeMap::new()));
        assert_eq!(p("!ytp mv info").unwrap().name, CommandName::Info);

        let c = p("!ytp mv lead=yeah kick=boom max=60").unwrap();
        assert_eq!(
            Value::Object(c.parts["lead"].clone()),
            json!({"text": "yeah"})
        );
        assert_eq!(
            Value::Object(c.parts["kick"].clone()),
            json!({"text": "boom"})
        );
        assert_eq!(Value::Object(c.options), json!({"max_seconds": 60.0}));

        let c = p("!ytp mv lead=\"oh no\" bass=hello_there").unwrap();
        assert_eq!(c.parts["lead"]["text"], json!("oh no"));
        assert_eq!(c.parts["bass"]["text"], json!("hello there"));

        let c = p("!ytp mv octave:bass=+1 mode:lead=tape take:kick=3 volume:hats=0.4")
            .unwrap();
        assert_eq!(jpart(&c, "bass"), json!({"octave": 1}));
        assert_eq!(jpart(&c, "lead"), json!({"mode": "tape"}));
        assert_eq!(jpart(&c, "kick"), json!({"take": 3}));
        assert_eq!(jpart(&c, "hats"), json!({"volume": 0.4}));

        let c = p("!ytp mv hats=mute").unwrap();
        assert_eq!(jpart(&c, "hats"), json!({"mute": true, "visible": false}));

        let c = p("!ytp mv vary=random jitter=off").unwrap();
        assert_eq!(Value::Object(c.options), json!({"vary": "random", "jitter": false}));
        assert_eq!(
            Value::Object(p("!ytp mv vary=ultra").unwrap().options),
            json!({"vary": "ultra"})
        );
        assert_eq!(
            Value::Object(p("!ytp mv chip=md").unwrap().options),
            json!({"chip": "md"})
        );
        assert_eq!(
            Value::Object(p("!ytp mv chip=md-voice").unwrap().options),
            json!({"chip": "md-voice"})
        );
        for (text, chip) in [
            ("nes", "nes"),
            ("sms", "sms"),
            ("snes", "snes"),
            ("snes-voice", "snes-voice"),
            ("opl2", "opl2"),
            ("sid8580", "sid8580"),
            ("gb", "gb"),
            ("gbc-voice", "gbc-voice"),
        ] {
            assert_eq!(
                Value::Object(p(&format!("!ytp mv chip={text}")).unwrap().options),
                json!({"chip": chip}),
                "{text}"
            );
        }
        assert_eq!(
            Value::Object(p("!ytp mv balance=off").unwrap().options),
            json!({"balance": false})
        );
        assert_eq!(
            Value::Object(p("!ytp mv chip=off").unwrap().options),
            json!({"chip": false})
        );
        assert_eq!(
            p("!ytp mv chip=amiga").unwrap().errors,
            vec![CHIP_ERROR.to_string()]
        );

        assert_eq!(jpart(&p("!ytp mv tone:lead=psg").unwrap(), "lead"), json!({"tone": "psg"}));
        assert_eq!(
            jpart(&p("!ytp mv tone:bass=gb-wave").unwrap(), "bass"),
            json!({"tone": "gb-wave"})
        );
        assert_eq!(
            jpart(&p("!ytp mv tone:bass=triangle").unwrap(), "bass"),
            json!({"tone": "triangle"})
        );
        assert_eq!(
            jpart(&p("!ytp mv tone:bass=organ").unwrap(), "bass"),
            json!({"tone": "organ"})
        );
        assert_eq!(
            jpart(&p("!ytp mv tone:bass=megadrive").unwrap(), "bass"),
            json!({"tone": "megadrive"})
        );
        assert_eq!(p("!ytp mv tone:bass=trombone").unwrap().errors.len(), 1);
        assert_eq!(
            jpart(&p("!ytp mv chip=nes tone:lead=voice").unwrap(), "lead"),
            json!({"tone": "voice"})
        );
        assert_eq!(
            jpart(&p("!ytp mv program:lead=29").unwrap(), "lead"),
            json!({"program": 29})
        );
        assert_eq!(p("!ytp mv program:lead=128").unwrap().errors.len(), 1);
        assert_eq!(
            p("!ytp mv vary=sideways").unwrap().errors,
            vec!["vary is one of off, random, rotate, ultra".to_string()]
        );

        let c = p("!ytp mv speed=1.5 key=-2 labels=on flip=off").unwrap();
        assert_eq!(
            Value::Object(c.options),
            json!({"speed": 1.5, "transpose": -2, "labels": true, "flip": false})
        );

        let c = p("!ytp mv mode:lead=loud octave:bass=up banana").unwrap();
        assert_eq!(c.errors.len(), 3, "{:?}", c.errors);
    }

    fn jpart(c: &Command, name: &str) -> Value {
        Value::Object(c.parts.get(name).cloned().unwrap_or_default())
    }

    #[test]
    fn other_verbs() {
        assert_eq!(p("!ytp voices").unwrap().name, CommandName::Voices);
        let c = p("!ytp voice dank-pods").unwrap();
        assert_eq!((c.name, c.text.as_str()), (CommandName::Voice, "dank-pods"));
        assert_eq!(p("!ytp voice").unwrap().name, CommandName::Voices);
        assert_eq!(p("!ytp queue").unwrap().name, CommandName::Queue);
        let c = p("!ytp dance").unwrap();
        assert_eq!((c.name, c.text.as_str()), (CommandName::Unknown, "dance"));
    }

    #[test]
    fn midi_files_and_parts() {
        assert!(is_midi(Some("E1M1.MID"), None));
        assert!(is_midi(None, Some("audio/midi")));
        assert!(!is_midi(Some("song.mp3"), Some("audio/mpeg")));

        let parts = vec![
            PartInfo {
                id: "t2c1".into(),
                name: "Overdriven Guitar (lead)".into(),
                role: "lead".into(),
                instrument: "Overdriven Guitar".into(),
            },
            PartInfo {
                id: "t1c0".into(),
                name: "Distortion Guitar (rhythm)".into(),
                role: "rhythm".into(),
                instrument: "Distortion Guitar".into(),
            },
            PartInfo {
                id: "t3c2".into(),
                name: "Picked Bass (bass)".into(),
                role: "bass".into(),
                instrument: "Picked Bass".into(),
            },
            PartInfo {
                id: "t4c9-hats".into(),
                name: "hats".into(),
                role: "drums:hats".into(),
                instrument: "Hi-hats".into(),
            },
        ];
        assert_eq!(match_part("bass", &parts).unwrap().id, "t3c2");
        assert_eq!(match_part("hats", &parts).unwrap().id, "t4c9-hats");
        assert_eq!(match_part("hihat", &parts).unwrap().id, "t4c9-hats");
        assert_eq!(match_part("t1c0", &parts).unwrap().id, "t1c0");
        assert_eq!(match_part("distortion", &parts).unwrap().id, "t1c0");
        assert_eq!(match_part("melody", &parts).unwrap().id, "t2c1");
        assert!(match_part("kazoo", &parts).is_none());
    }
}
