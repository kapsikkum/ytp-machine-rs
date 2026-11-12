import { TRACK_COLORS, type Note, type Track } from '#lib/timeline.ts';
import type { AltTake, MidiAnalysis, PartAnalysis } from '#lib/types.ts';

// What the YTPMV page works on: a MIDI file as the server read it, plus the
// choices made for each part. Only what differs from the server's suggestion
// is sent back, so the suggestion (and its rotating takes) keeps working.

export interface Part {
  id: string;
  name: string;
  role: string;
  /** the General MIDI instrument the file names */
  program: string;
  isDrums: boolean;
  noteCount: number;
  notes: Note[];
  /** the word / phrase the voice sings for this part ('' = nothing found) */
  text: string;
  take: number;
  /** '' follows the song default voice */
  corpus: string;
  /** '' follows the global chip */
  tone: string;
  octave: number;
  /** percent */
  volume: number;
  mute: boolean;
  solo: boolean;
  score: number | null;
  alternatives: AltTake[];
  /** the server's suggestion, to tell what has been changed */
  base: { text: string; take: number; octave: number; volume: number; mute: boolean };
}

export interface Song {
  midiId: string;
  title: string;
  description: string;
  bpm: number;
  key: string;
  duration: number;
  parts: Part[];
  analysis: MidiAnalysis;
}

export const CHIPS = [
  '', 'md', 'md-voice', 'nes', 'nes-voice', 'sms', 'sms-voice', 'snes', 'snes-voice', 'gb', 'gb-voice', 'gbc', 'gbc-voice',
  'opl2', 'opl2-voice', 'opl3', 'opl3-voice', 'sid', 'sid-voice', 'sid8580', 'sid8580-voice'
];

export const LAYOUTS: [string, string][] = [
  ['balanced', 'Balanced grid'],
  ['classic', 'Classic grid'],
  ['two-rows', 'Two rows'],
  ['two-columns', 'Two columns'],
  ['strips', 'Vertical strips'],
  ['featured', 'Featured track']
];

function notesOf(p: PartAnalysis): Note[] {
  const [lo, hi] = p.range_midi;
  const span = Math.max(1, hi - lo);
  return p.activity.map(([t, d, pitch]) => ({ t, d, p: 0.12 + 0.76 * Math.min(1, Math.max(0, (pitch - lo) / span)) }));
}

function partOf(p: PartAnalysis): Part {
  const s = p.settings;
  const text = s.text ?? '';
  const volume = Math.round(s.volume * 100);
  return {
    id: p.id,
    name: p.name,
    role: p.role,
    program: p.instrument,
    isDrums: p.is_drums,
    noteCount: p.notes,
    notes: notesOf(p),
    text,
    take: s.take,
    corpus: '',
    tone: '',
    octave: s.octave,
    volume,
    mute: s.mute,
    solo: false,
    score: p.recommended.score,
    alternatives: p.recommended.alternatives,
    base: { text, take: s.take, octave: s.octave, volume, mute: s.mute }
  };
}

export function fromAnalysis(a: MidiAnalysis): Song {
  return {
    midiId: a.midi_id,
    title: a.title || 'Untitled MIDI',
    description: a.description,
    bpm: a.bpm,
    key: a.key,
    duration: a.duration,
    parts: a.parts.map(partOf),
    analysis: a
  };
}

/** A refreshed suggestion for one part (after its voice changed). */
export function suggest(part: Part, rec: { text: string | null; take: number; score: number | null; alternatives: AltTake[] }, octave: number): void {
  part.text = rec.text ?? '';
  part.take = rec.take;
  part.score = rec.score;
  part.alternatives = rec.alternatives;
  part.octave = octave;
  part.base.text = part.text;
  part.base.take = part.take;
  part.base.octave = octave;
}

/** The `parts` of a preview or render request: only what was changed. */
export function serverParts(song: Song): Record<string, unknown>[] {
  const anySolo = song.parts.some((p) => p.solo);
  return song.parts.map((p) => {
    const o: Record<string, unknown> = { id: p.id };
    if (p.corpus) o.corpus = p.corpus;
    const text = p.text.trim();
    if (text && text !== p.base.text) o.text = text;
    if (p.take !== p.base.take || (text && text !== p.base.text)) o.take = p.take;
    if (p.tone) o.tone = p.tone;
    if (p.octave !== p.base.octave) o.octave = p.octave;
    if (p.volume !== p.base.volume) o.volume = p.volume / 100;
    const mute = anySolo ? !p.solo : p.mute;
    if (mute !== p.base.mute || anySolo) o.mute = mute;
    return o;
  });
}

/** The tone a part plays with while it follows the chip. */
export function chipToneOf(a: PartAnalysis, chip: string, voice: boolean): string {
  if (!chip) return 'the voice';
  return (voice ? a.chip_tones : a.chip_tones_synth)[chip] ?? 'the voice';
}

export function toTracks(song: Song, defaultVoice: string, chip: string): Track[] {
  return song.parts.map((p, i) => {
    const a = song.analysis.parts[i];
    const sound = p.tone || (chip && a ? chipToneOf(a, chip, true) : '');
    const sub = sound || `${p.text || 'no word'} · ${p.corpus || defaultVoice || 'no voice'}`;
    return {
      id: p.id,
      name: p.name,
      kind: 'midi',
      sub: `${p.program} · ${sub}`,
      color: TRACK_COLORS[i % TRACK_COLORS.length],
      mute: p.mute,
      solo: p.solo,
      notes: p.notes
    };
  });
}
