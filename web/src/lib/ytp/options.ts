// What the generator accepts. Ported from index.html (the Python app's OPTIONS).
// Kept in this browser, sent with every sentence.

export interface SliderDef {
  key: string;
  name: string;
  min: number;
  max: number;
  step: number;
  def: number;
  fmt: (v: number) => string;
  hint: string;
}

export interface ToggleDef {
  key: string;
  name: string;
  def: boolean;
  hint: string;
}

export type Options = Record<string, number | boolean | string>;

export const SLIDERS: SliderDef[] = [
  { key: 'speed', name: 'speed', min: 0.5, max: 2, step: 0.05, def: 1, fmt: (v) => `${v.toFixed(2)}×`,
    hint: 'The whole video faster or slower. The voice keeps its pitch.' },
  { key: 'pitch', name: 'pitch', min: -12, max: 12, step: 1, def: 0, fmt: (v) => (v === 0 ? 'as recorded' : `${v > 0 ? '+' : ''}${v} semitones`),
    hint: 'Higher or lower, at the same speed.' },
  { key: 'word_gap', name: 'gap between words', min: 0, max: 0.6, step: 0.01, def: 0.03, fmt: (v) => `${Math.round(v * 1000)} ms`,
    hint: 'A held frame and a moment of silence between separate words.' },
  { key: 'sentence_pause', name: 'pause at a full stop', min: 0, max: 2, step: 0.05, def: 0.5, fmt: (v) => `${v.toFixed(2)} s`,
    hint: 'How long a . ! or ? waits before the next sentence.' },
  { key: 'chaos', name: 'chaos', min: 0, max: 1, step: 0.05, def: 0, fmt: (v) => (v ? `${Math.round(v * 100)}% of words` : 'none'),
    hint: 'How many words get wrecked at random: shaking, colours, earrape, stutters, boomerangs. The YTP knob.' },
  { key: 'pause_style', name: 'at a full stop', min: 0, max: 2, step: 1, def: 0, fmt: (v) => ['silent clip', 'freeze frame', 'no pause'][v] ?? String(v),
    hint: 'What happens for the length of the full-stop pause: footage of the speaker being quiet, the last frame held still, or straight on to the next sentence.' },
  { key: 'tone_match', name: 'same tone', min: 0, max: 1, step: 0.05, def: 0.6, fmt: (v) => (v <= 0 ? 'off' : v >= 1 ? 'strict' : `${Math.round(v * 100)}%`),
    hint: 'Prefer takes said in the same tone of voice — pitch, loudness, pace — and from the same recording, so a sentence does not jump between moods. Needs a corpus whose tone has been measured.' },
  { key: 'stretch', name: 'held letter', min: 0.02, max: 0.35, step: 0.01, def: 0.09, fmt: (v) => `${Math.round(v * 1000)} ms a letter`,
    hint: 'Type a letter several times — loooong — and it is held this much longer for every extra one.' }
];

export const TOGGLES: ToggleDef[] = [
  { key: 'clean_takes', name: 'clean takes', def: true,
    hint: 'Prefer recordings of a word said before a pause. Far fewer clipped endings and bits of the next word, at the cost of some variety.' },
  { key: 'phrases', name: 'real phrases', def: true,
    hint: 'When several words were said together somewhere, play that recording instead of cutting word by word. Off is choppier.' }
];

export const DEFAULTS: Options = Object.fromEntries([...SLIDERS, ...TOGGLES].map((o) => [o.key, o.def]));

export const PRESETS: { name: string; opts: Options }[] = [
  { name: 'natural', opts: {} },
  { name: 'choppy', opts: { phrases: false, clean_takes: false, tone_match: 0, word_gap: 0.12, sentence_pause: 0.3 } },
  { name: 'rapid fire', opts: { speed: 1.35, word_gap: 0, sentence_pause: 0.15 } },
  { name: 'chipmunk', opts: { pitch: 7, speed: 1.2 } },
  { name: 'slow and low', opts: { pitch: -5, speed: 0.75, sentence_pause: 0.9 } },
  { name: 'drawn out', opts: { stretch: 0.2, word_gap: 0.08, speed: 0.9 } },
  { name: 'chickenpika', opts: { chaos: 0.55, speed: 1.3, word_gap: 0, sentence_pause: 0.15, phrases: false } }
];

/** Defaults plus `o`, keeping only options this build knows (saved ones from older versions, such as sing and vocode, are dropped: the server refuses them). */
export function withDefaults(o: Options): Options {
  const known = Object.fromEntries(Object.entries(o).filter(([k]) => k in DEFAULTS));
  return { ...DEFAULTS, ...known };
}

export function matchesPreset(opts: Options, preset: { opts: Options }): boolean {
  return Object.keys(DEFAULTS).every((k) => (k in preset.opts ? preset.opts[k] : DEFAULTS[k]) === opts[k]);
}

/** A word typed as ~word~ is played backwards; the chips mark it. */
export function chipsOf(text: string): { word: string; reversed: boolean }[] {
  return text
    .split(/\s+/)
    .filter(Boolean)
    .map((w) => ({ word: w.replace(/^~|~$/g, ''), reversed: /^~.+~$/.test(w) }));
}
