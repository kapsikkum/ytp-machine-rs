//! `/api/ytpmv/*`: a MIDI file played by a corpus. Upload one to have it read
//! and a sound suggested for every part; audition parts; hear a quick
//! audio-only mix; then render a video through the same queue as sentences.
//!
//! The previews are synchronous and in-process: they answer with the WAV
//! itself (`audio/wav`), with what happened in the `X-Ytpmv-Info` header.

use axum::extract::{Multipart, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use ytp_auth::Permission;
use ytp_core::Error;
use ytp_music::music::Song;
use ytp_music::wav;
use ytp_ytpmv::mix::{self, Request};
use ytp_ytpmv::recommend::recommend;
use ytp_ytpmv::samples::Banks;
use ytp_ytpmv::settings::{truthy, Options};
use ytp_ytpmv::{song as songs, MAX_SECONDS};

use crate::auth::CurrentUser;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::jobs::NewJob;
use crate::ytpmv::{api_error, ascii_json, MAX_MIDI_BYTES};
use crate::AppState;

const MAX_PARTS: usize = 256;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> ytp_core::Result<T> + Send + 'static) -> ApiResult<T> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(e) => Err(Error::internal(e).into()),
    }
}

/// The corpus to use when a part names none: the one asked for, else the
/// first installed. `None` when nothing is installed.
async fn default_voice(st: &AppState, requested: Option<&str>) -> ApiResult<Option<String>> {
    match requested.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => {
            st.corpora
                .get(v)
                .await
                .map_err(|_| Error::invalid(format!("The corpus '{v}' is not installed. Choose another voice.")))?;
            Ok(Some(v.to_string()))
        }
        None => Ok(st.corpora.list().await?.into_iter().next().map(|c| c.name)),
    }
}

async fn load_banks(st: &AppState, names: &[String]) -> ApiResult<Banks> {
    let mut banks = Banks::new();
    for name in names {
        if name.is_empty() || banks.contains_key(name) {
            continue;
        }
        let pool = st
            .jobs
            .pool(st, name)
            .await
            .map_err(|_| Error::invalid(format!("The corpus '{name}' is not installed. Choose another voice.")))?;
        banks.insert(name.clone(), st.ytpmv.bank(name, pool));
    }
    Ok(banks)
}

async fn song_of(st: &AppState, me: &CurrentUser, id: &str) -> ApiResult<std::sync::Arc<Song>> {
    let (st2, id, owner) = (st.clone(), id.to_string(), me.user.id);
    blocking(move || st2.ytpmv.load_song(owner, &id)).await
}

fn wav_response(bytes: Vec<u8>, info: &Value) -> Response {
    let mut res = (StatusCode::OK, bytes).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/wav"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&ascii_json(info)) {
        h.insert("x-ytpmv-info", v);
    }
    res
}

fn str_field<'a>(body: &'a Value, key: &str) -> ApiResult<&'a str> {
    body.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid(format!("`{key}` is required")).into())
}

fn parts_field(body: &Value) -> ApiResult<Vec<Value>> {
    match body.get("parts") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) if a.len() <= MAX_PARTS => Ok(a.clone()),
        Some(Value::Array(_)) => Err(Error::invalid(format!("at most {MAX_PARTS} parts")).into()),
        Some(_) => Err(Error::invalid("`parts` must be a list").into()),
    }
}

fn options_field(body: &Value) -> ApiResult<Value> {
    match body.get("options") {
        None | Some(Value::Null) => Ok(json!({})),
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        Some(_) => Err(Error::invalid("`options` must be an object").into()),
    }
}

// ── The library ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct VoiceQuery {
    #[serde(default, alias = "corpus")]
    pub voice: Option<String>,
}

/// Describe a song and suggest a sound for every part.
async fn describe(st: &AppState, me: &CurrentUser, id: &str, voice: Option<&str>) -> ApiResult<Value> {
    let song = song_of(st, me, id).await?;
    let corpus = default_voice(st, voice).await?;
    let banks = match &corpus {
        Some(c) => load_banks(st, std::slice::from_ref(c)).await?,
        None => Banks::new(),
    };
    let (st2, id) = (st.clone(), id.to_string());
    blocking(move || {
        let env = st2.ytpmv.env(&st2);
        let bank = corpus.as_deref().and_then(|c| banks.get(c));
        let (recs, noises, units, name) = match bank {
            Some(b) => {
                let recs = recommend(&song, b, &env, &st2.ytpmv.store, &|_, _, _| true).map_err(api_error)?;
                let mut units: Vec<(String, usize)> = b.units().iter().map(|(k, v)| (k.clone(), v.len())).collect();
                units.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                (recs, b.noises(), units, b.name.clone())
            }
            None => (Vec::new(), Vec::new(), Vec::new(), String::new()),
        };
        Ok(songs::analyse(&song, &id, &recs, &name, &noises, &units))
    })
    .await
}

/// Multipart: `midi` (the file; `file` also works), optional `voice`.
pub async fn upload(State(st): State<AppState>, me: CurrentUser, mut form: Multipart) -> ApiResult<Json<Value>> {
    let bad = |e: axum::extract::multipart::MultipartError| ApiError::bad_request(e.body_text());
    let mut file: Option<(String, Vec<u8>)> = None;
    let mut voice: Option<String> = None;
    while let Some(field) = form.next_field().await.map_err(bad)? {
        match field.name() {
            Some("midi") | Some("file") if file.is_none() => {
                let name = field.file_name().unwrap_or("").to_string();
                let bytes = field.bytes().await.map_err(bad)?;
                if bytes.len() > MAX_MIDI_BYTES {
                    return Err(Error::invalid("That MIDI file is too big (2 MB is the limit).").into());
                }
                file = Some((name, bytes.to_vec()));
            }
            Some("voice") | Some("corpus") => voice = Some(field.text().await.map_err(bad)?),
            _ => {}
        }
    }
    let Some((name, data)) = file else { return Err(ApiError::bad_request("missing `midi` field")) };
    let (st2, owner) = (st.clone(), me.user.id);
    let id = blocking(move || st2.ytpmv.save(owner, &data, &name)).await?;
    Ok(Json(describe(&st, &me, &id, voice.as_deref()).await?))
}

pub async fn library(State(st): State<AppState>, me: CurrentUser) -> ApiResult<Json<Vec<Value>>> {
    let owner = me.user.id;
    Ok(Json(blocking(move || Ok(st.ytpmv.library(owner))).await?))
}

pub async fn get_midi(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<VoiceQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(describe(&st, &me, &id, q.voice.as_deref()).await?))
}

#[derive(Deserialize)]
pub struct RenameBody {
    pub title: String,
}

pub async fn rename(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiPath(id): ApiPath<String>,
    ApiJson(body): ApiJson<RenameBody>,
) -> ApiResult<Json<Value>> {
    let owner = me.user.id;
    Ok(Json(blocking(move || st.ytpmv.rename(owner, &id, &body.title)).await?))
}

pub async fn delete(State(st): State<AppState>, me: CurrentUser, ApiPath(id): ApiPath<String>) -> ApiResult<StatusCode> {
    let owner = me.user.id;
    if st.jobs.midi_in_use(owner, &id) {
        return Err(Error::conflict("This MIDI is in use. Cancel its render before deleting it.").into());
    }
    blocking(move || st.ytpmv.delete(owner, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct NotesQuery {
    #[serde(default)]
    pub start: Option<f64>,
    #[serde(default)]
    pub end: Option<f64>,
    #[serde(default)]
    pub parts: Option<String>,
}

/// Bounded, accurate note detail for the visible timeline tracks and range.
pub async fn notes(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<NotesQuery>,
) -> ApiResult<Json<Value>> {
    let parts = q.parts.unwrap_or_default();
    if parts.len() > 4096 {
        return Err(ApiError::bad_request("Too many timeline parts."));
    }
    let ids: Vec<String> = parts.split(',').filter(|p| !p.is_empty()).map(str::to_string).collect();
    let song = song_of(&st, &me, &id).await?;
    let v = blocking(move || songs::timeline_notes(&song, q.start.unwrap_or(0.0), q.end, &ids).map_err(api_error)).await?;
    Ok(Json(v))
}

/// Refresh one part's recommendations and sounds for the chosen voice.
pub async fn part_voice(
    State(st): State<AppState>,
    me: CurrentUser,
    ApiPath((id, part_id)): ApiPath<(String, String)>,
    ApiQuery(q): ApiQuery<VoiceQuery>,
) -> ApiResult<Json<Value>> {
    let song = song_of(&st, &me, &id).await?;
    let Some(part) = song.part(&part_id).cloned() else { return Err(Error::invalid("No such part.").into()) };
    let Some(corpus) = default_voice(&st, q.voice.as_deref()).await? else {
        return Err(Error::invalid("no corpus is installed").into());
    };
    let banks = load_banks(&st, std::slice::from_ref(&corpus)).await?;
    let st2 = st.clone();
    let v = blocking(move || {
        let env = st2.ytpmv.env(&st2);
        let bank = &banks[&corpus];
        let mut one = (*song).clone();
        one.parts = vec![part.clone()];
        let recs = recommend(&one, bank, &env, &st2.ytpmv.store, &|_, _, _| true).map_err(api_error)?;
        let rec = recs.into_iter().next().map(|(_, r)| r).unwrap_or_else(|| ytp_ytpmv::recommend::Rec::empty(&part));
        let mut units: Vec<(String, usize)> = bank.units().iter().map(|(k, v)| (k.clone(), v.len())).collect();
        units.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        Ok(json!({
            "id": part.id, "corpus": corpus, "voice": corpus,
            "recommended": rec.to_json(),
            "settings": ytp_ytpmv::settings::default_settings_for(&part, &rec, &corpus).to_json(),
            "noises": bank.noises().into_iter().collect::<std::collections::BTreeMap<_, _>>(),
            "units": units.into_iter().collect::<std::collections::BTreeMap<_, _>>(),
        }))
    })
    .await?;
    Ok(Json(v))
}

// ── Samples ──────────────────────────────────────────────────────────────────

async fn build_sample(st: &AppState, body: &Value) -> ApiResult<std::sync::Arc<ytp_ytpmv::samples::Sample>> {
    let text = str_field(body, "text")?.to_string();
    if text.chars().count() > 120 {
        return Err(Error::invalid("That is too long for one sample.").into());
    }
    let take = body.get("take").and_then(Value::as_u64).unwrap_or(0).min(10_000) as usize;
    let hit = body.get("hit").and_then(Value::as_str).filter(|h| !h.is_empty()).map(str::to_string);
    let requested = body.get("corpus").and_then(Value::as_str);
    let Some(corpus) = default_voice(st, requested).await? else { return Err(Error::invalid("no corpus is installed").into()) };
    let banks = load_banks(st, std::slice::from_ref(&corpus)).await?;
    let st2 = st.clone();
    blocking(move || {
        let env = st2.ytpmv.env(&st2);
        banks[&corpus].build(&env, &text, take, hit.as_deref(), &|| true).map_err(api_error)
    })
    .await
}

/// What a word sounds like as an instrument: the clip, and the note it is on.
pub async fn sample(State(st): State<AppState>, _me: CurrentUser, ApiJson(body): ApiJson<Value>) -> ApiResult<Json<Value>> {
    Ok(Json(build_sample(&st, &body).await?.describe()))
}

/// The sample itself, trimmed and shaped as it is played: a mono WAV.
pub async fn sample_audio(State(st): State<AppState>, _me: CurrentUser, ApiJson(body): ApiJson<Value>) -> ApiResult<Response> {
    let s = build_sample(&st, &body).await?;
    let samples = wav::quantize_f32(&s.voice.x);
    let bytes = wav::write(&wav::Wav { sample_rate: s.voice.sr, channels: 1, samples });
    Ok(wav_response(bytes, &s.describe()))
}

// ── Previews ─────────────────────────────────────────────────────────────────

/// A few seconds of one part, sung as it will be in the render (audio only).
pub async fn preview(State(st): State<AppState>, me: CurrentUser, ApiJson(body): ApiJson<Value>) -> ApiResult<Response> {
    let id = str_field(&body, "midi_id")?.to_string();
    let song = song_of(&st, &me, &id).await?;
    let part = body.get("part").filter(|p| p.is_object()).cloned().ok_or_else(|| Error::invalid("`part` is required"))?;
    let seconds = body.get("seconds").and_then(Value::as_f64).unwrap_or(6.0).clamp(1.0, 15.0);
    let synth = truthy(body.get("synth"));
    let mut o = json!({ "synth": synth });
    if let Some(c) = body.get("chip") {
        o["chip"] = c.clone();
    }
    let options = Options::from_json(&o, MAX_SECONDS);
    let requested = body.get("corpus").and_then(Value::as_str).or(part.get("corpus").and_then(Value::as_str));
    let corpus = default_voice(&st, requested).await?;
    if corpus.is_none() && !synth {
        return Err(Error::invalid("no corpus is installed").into());
    }
    let default = corpus.unwrap_or_default();
    let banks = if synth { Banks::new() } else { load_banks(&st, &mix::corpora_of(std::slice::from_ref(&part), &default)).await? };
    let st2 = st.clone();
    let (bytes, info) = blocking(move || {
        let env = st2.ytpmv.env(&st2);
        let req = Request {
            song: &song,
            parts: &[],
            options,
            default_corpus: &default,
            banks: &banks,
            env: &env,
            store: &st2.ytpmv.store,
            audition: None,
            audio_only: true,
            start_seconds: 0.0,
            only_part: None,
        };
        mix::preview_part(&req, &part, seconds, &|_, _, _| true).map_err(api_error)
    })
    .await?;
    Ok(wav_response(bytes, &info))
}

/// Play a selection (or the rest of the song) through the video render's mixer,
/// without the picture. The answer is the WAV.
pub async fn preview_mix(State(st): State<AppState>, me: CurrentUser, ApiJson(body): ApiJson<Value>) -> ApiResult<Response> {
    let id = str_field(&body, "midi_id")?.to_string();
    let song = song_of(&st, &me, &id).await?;
    let mut parts = parts_field(&body)?;
    let options = options_field(&body)?;
    let speed = options.get("speed").and_then(Value::as_f64).unwrap_or(1.0).clamp(0.25, 4.0);
    let mut start = body.get("start").and_then(Value::as_f64).filter(|s| s.is_finite()).unwrap_or(0.0).clamp(0.0, MAX_SECONDS);
    let part_id = body.get("part_id").and_then(Value::as_str).map(str::to_string);
    let note = body.get("note").filter(|n| !n.is_null());
    let notes = body.get("notes").filter(|n| !n.is_null());
    let midi_note = |v: &Value| v.as_u64().filter(|n| *n <= 127).map(|n| n as u8);

    let mut audition: Option<(String, Vec<u8>)> = None;
    if let Some(n) = note {
        let (Some(p), Some(n)) = (part_id.clone(), midi_note(n)) else {
            return Err(Error::invalid("Choose a track and a MIDI note from 0 to 127.").into());
        };
        audition = Some((p, vec![n]));
    }
    if let Some(list) = notes {
        let picked: Option<Vec<u8>> = list.as_array().and_then(|a| a.iter().map(midi_note).collect());
        let ok = picked.as_ref().is_some_and(|v| {
            (1..=24).contains(&v.len()) && v.iter().collect::<std::collections::HashSet<_>>().len() == v.len()
        });
        let (Some(p), true, true) = (part_id.clone(), ok, note.is_none()) else {
            return Err(Error::invalid("Choose a track and 1–24 distinct keyboard notes from 0 to 127.").into());
        };
        audition = Some((p, picked.unwrap()));
    }
    let keyboard = notes.and_then(Value::as_array).map(Vec::len);
    if let Some(pid) = &part_id {
        let Some(part) = song.part(pid) else { return Err(Error::invalid("No such part.").into()) };
        start = (part.notes.first().map_or(0.0, |n| n.start / speed) - 0.05).max(0.0);
        if audition.is_some() {
            start = 0.0;
        }
        // Only this part sounds: the rest are muted and hidden.
        let given = parts.clone();
        parts = song
            .parts
            .iter()
            .map(|p| {
                let mut v = given.iter().find(|g| g.get("id").and_then(Value::as_str) == Some(&p.id)).cloned().unwrap_or_else(|| json!({ "id": p.id }));
                v["mute"] = json!(&p.id != pid);
                v["visible"] = json!(false);
                v
            })
            .collect();
    }
    let seconds = match (&part_id, body.get("seconds")) {
        (Some(_), s) => Some(s.and_then(Value::as_f64).unwrap_or(10.0).clamp(1.0, 15.0)),
        (None, Some(Value::Null)) => None,
        (None, s) => Some(s.and_then(Value::as_f64).unwrap_or(10.0).clamp(1.0, MAX_SECONDS)),
    };
    let opts = mix::preview_options(&options, seconds, keyboard);

    let corpus = default_voice(&st, body.get("voice").and_then(Value::as_str)).await?;
    if corpus.is_none() && !opts.synth {
        return Err(Error::invalid("no corpus is installed").into());
    }
    let default = corpus.unwrap_or_default();
    let banks = if opts.synth { Banks::new() } else { load_banks(&st, &mix::corpora_of(&parts, &default)).await? };
    let st2 = st.clone();
    let (bytes, info) = blocking(move || {
        let env = st2.ytpmv.env(&st2);
        let req = Request {
            song: &song,
            parts: &parts,
            options: opts,
            default_corpus: &default,
            banks: &banks,
            env: &env,
            store: &st2.ytpmv.store,
            audition,
            audio_only: true,
            start_seconds: start,
            only_part: None,
        };
        let progress = |_: &str, _: usize, _: usize| true;
        let prep = mix::prepare(&req, &progress).map_err(api_error)?;
        let mixed = mix::mix(&prep, &progress).map_err(api_error)?;
        let duration = prep.t_end - prep.t_from;
        let mut info = json!({
            "from": ytp_ytpmv::round_to(prep.t_from, 2),
            "duration": ytp_ytpmv::round_to(duration, 2),
            "problems": prep.problems.iter().take(20).collect::<Vec<_>>(),
            "options": prep.opts.to_json(),
            "parts": mix::playing_report(&prep),
        });
        if let Some(k) = keyboard {
            let notes = req.audition.as_ref().map(|a| a.1.clone()).unwrap_or_default();
            info["keyboard"] = Value::Array(
                notes
                    .iter()
                    .enumerate()
                    .take(k)
                    .map(|(i, n)| json!({ "note": n, "offset": i * 2, "duration": (duration - (i * 2) as f64).min(2.0) }))
                    .collect(),
            );
        }
        Ok((mixed.wav(), info))
    })
    .await?;
    Ok(wav_response(bytes, &info))
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Queue a render of the whole song to video; follow it as a job.
pub async fn render(State(st): State<AppState>, me: CurrentUser, ApiJson(body): ApiJson<Value>) -> ApiResult<Json<Value>> {
    if !me.has(Permission::ExportRender) {
        return Err(ApiError::forbidden("missing permission export.render"));
    }
    let id = str_field(&body, "midi_id")?.to_string();
    let song = song_of(&st, &me, &id).await?;
    let parts = parts_field(&body)?;
    let options = options_field(&body)?;
    let synth = truthy(options.get("synth"));
    let corpus = default_voice(&st, body.get("voice").and_then(Value::as_str)).await?;
    if corpus.is_none() && !synth {
        return Err(Error::invalid("no corpus is installed").into());
    }
    // Every voice a part asks for must be installed before it is queued.
    if !synth {
        for p in &parts {
            if let Some(c) = p.get("corpus").and_then(Value::as_str).filter(|c| !c.trim().is_empty()) {
                st.corpora
                    .get(c)
                    .await
                    .map_err(|_| Error::invalid(format!("The corpus '{c}' is not installed. Choose another voice.")))?;
            }
        }
    }
    let title = if song.title.is_empty() { format!("midi {id}") } else { song.title.clone() };
    let rec = st.jobs.submit_as(
        &me,
        NewJob {
            text: title,
            voice: corpus.unwrap_or_default(),
            captions: false,
            options: json!({ "midi_id": id, "parts": parts, "options": options }),
        },
        Some("ytpmv"),
    )?;
    Ok(Json(crate::jobs::accepted(&rec)))
}
