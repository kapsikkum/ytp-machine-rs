# Glossary

Short glossary of the terms used in the code and docs.

| Term | Meaning |
|---|---|
| YTP | YouTube Poop: here, a video of typed text "said" by splicing word clips from a corpus. |
| YTPMV | YouTube Poop Music Video: a MIDI song played with corpus word clips and chip instruments, rendered to video. |
| corpus | One speaker's recorded speech: source videos, word clips with timings and alignments, plus settings. |
| corpus bundle (.tar.zst) | A compressed archive of one corpus, built by the Python tools and installed by the server. |
| clip | A time span of a source video where one word is spoken. |
| phoneme splice | Building a word the corpus lacks from phonemes cut out of other words. |
| splice recipe | A saved, hand-picked list of pieces (clip spans) that spell a word, used instead of the automatic splice. |
| pronunciation override | An entry in `pronunciations.csv` giving a word's phonemes, so lookup and splicing use them. |
| golden fixture | A stored output captured from the Python app (JSON or WAV); Rust tests must reproduce it. |
| chip | An emulated sound chip used as a YTPMV instrument (NES, GB, SID, OPL, YM2612, SMS, SNES). |
| master | The high-quality intermediate render kept beside a job's result, so exports re-encode it without re-rendering. |
| preview-proxy | An encode preset making low-res, all-keyframe copies so the Studio timeline can scrub and preview fast. |
| encode preset | A named, data-defined ffmpeg recipe (codec, quality or target size, resolution cap). |
| job | A queued render or export, with progress sent over the WebSocket until it finishes. |
| role | A named access level (`admin`, `editor`, `creator`, `guest`) holding permissions and quotas. |
| permission | A string scope such as `generate.ytp` or `corpus.install`. |
| API token | A scoped secret (`ytp_...`) sent as `Authorization: Bearer` by scripts and bots instead of logging in. |
| Studio | The main editing interface at `/`; the simple `/ytp` and `/ytpmv` pages stay alongside it. |
