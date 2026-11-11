import { TRACK_COLORS, type Note, type Track } from '#lib/timeline.ts';

// What the YTPMV page works on. The MIDI endpoints answer 501 for now, so a
// song only exists here as the sample below, for looking at the layout.

export interface Part {
  id: string;
  name: string;
  role: 'lead' | 'bass' | 'drums' | 'pad';
  program: string;
  notes: Note[];
  /** the word / phrase the voice sings for this part */
  text: string;
  take: number;
  /** '' follows the song default voice */
  corpus: string;
  /** 'auto' follows the global chip */
  instrument: string;
  octave: number;
  volume: number;
  mute: boolean;
  solo: boolean;
}

export interface Song {
  title: string;
  description: string;
  bpm: number;
  key: string;
  duration: number;
  sample: boolean;
  parts: Part[];
}

export const CHIPS = [
  '', 'md', 'md-voice', 'nes', 'nes-voice', 'sms', 'sms-voice', 'snes', 'snes-voice', 'gb', 'gb-voice', 'gbc', 'gbc-voice',
  'opl2', 'opl2-voice', 'opl3', 'opl3-voice', 'sid', 'sid-voice', 'sid8580', 'sid8580-voice'
];

export const INSTRUMENTS = ['auto', 'voice', 'square', 'triangle', 'noise', 'fm', 'sample'];

export const LAYOUTS: [string, string][] = [
  ['balanced', 'Balanced grid'],
  ['classic', 'Classic grid'],
  ['two-rows', 'Two rows'],
  ['two-columns', 'Two columns'],
  ['strips', 'Vertical strips'],
  ['featured', 'Featured track']
];

function pattern(seed: number, count: number, every: number, spread: number, length: number): Note[] {
  const out: Note[] = [];
  let s = seed;
  for (let i = 0; i < count; i++) {
    s = (s * 1103515245 + 12345) & 0x7fffffff;
    out.push({ t: i * every, d: length, p: 0.15 + ((s % 1000) / 1000) * spread });
  }
  return out;
}

export function sampleSong(): Song {
  return {
    title: 'Sample song',
    description: 'A made-up arrangement, only here to show the layout.',
    bpm: 120,
    key: 'C major',
    duration: 32,
    sample: true,
    parts: [
      { id: 'p1', name: 'Lead', role: 'lead', program: 'Square lead', notes: pattern(3, 48, 0.5, 0.7, 0.4), text: 'hello', take: 0, corpus: '', instrument: 'auto', octave: 0, volume: 100, mute: false, solo: false },
      { id: 'p2', name: 'Bass', role: 'bass', program: 'Fingered bass', notes: pattern(7, 32, 1, 0.3, 0.8), text: 'boom', take: 0, corpus: '', instrument: 'auto', octave: -1, volume: 90, mute: false, solo: false },
      { id: 'p3', name: 'Drums', role: 'drums', program: 'Standard kit', notes: pattern(11, 64, 0.5, 0.2, 0.1), text: 'pop', take: 0, corpus: '', instrument: 'noise', octave: 0, volume: 80, mute: false, solo: false },
      { id: 'p4', name: 'Pad', role: 'pad', program: 'Warm pad', notes: pattern(19, 8, 4, 0.5, 3.6), text: 'oooh', take: 0, corpus: '', instrument: 'auto', octave: 0, volume: 70, mute: false, solo: false }
    ]
  };
}

export function toTracks(song: Song, defaultVoice: string): Track[] {
  return song.parts.map((p, i) => ({
    id: p.id,
    name: p.name,
    kind: 'midi',
    sub: p.instrument === 'auto' ? `${p.program} · ${p.corpus || defaultVoice || 'no voice'}` : p.instrument,
    color: TRACK_COLORS[i % TRACK_COLORS.length],
    mute: p.mute,
    solo: p.solo,
    notes: p.notes
  }));
}
