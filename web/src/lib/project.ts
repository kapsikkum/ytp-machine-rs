import { TRACK_COLORS, type Track } from './timeline';

// The timeline model as the API speaks it (crates/ytp-core/src/timeline.rs).

export type Effect =
  | { type: 'speed'; factor: number }
  | { type: 'stretch'; factor: number }
  | { type: 'reverse' }
  | { type: 'pitch'; semitones: number }
  | { type: 'volume'; db: number }
  | { type: 'stutter' }
  | { type: 'mirror' }
  | { type: 'glitch'; name: string }
  | { type: 'fade_in'; seconds: number }
  | { type: 'fade_out'; seconds: number }
  | { type: 'pause'; seconds: number }
  | { type: 'subword' };

export type ClipSource =
  | { type: 'corpus_clip'; corpus: string; clip_id?: number; file?: string; word?: string; take?: number; in: number; out: number }
  | { type: 'media'; asset: string; in: number; out: number }
  | { type: 'synth'; chip: string; notes: { start: number; dur: number; pitch: number; velocity: number }[] }
  | { type: 'generated'; kind: 'ytp' | 'ytpmv'; prompt: string; options: unknown; seed: string; corpus?: string };

export interface ProjectClip {
  id: string;
  start: number;
  duration: number;
  source: ClipSource;
  effects: Effect[];
  label?: string;
  caption?: { text: string; words: [string, number, number][] };
}

export interface ProjectTrack {
  id: string;
  kind: 'video' | 'audio';
  name: string;
  mute: boolean;
  solo: boolean;
  volume: number;
  part?: string;
  generated: boolean;
  clips: ProjectClip[];
}

export interface Project {
  version: number;
  id?: number | null;
  owner?: number | null;
  name: string;
  corpus: string;
  fps: number;
  width: number;
  height: number;
  captions: boolean;
  generator?: { kind: 'ytp' | 'ytpmv'; request: unknown; seed: string };
  tracks: ProjectTrack[];
}

export type Access = 'owner' | 'write' | 'read';

/** `GET /api/projects/:id`: the document with its row. */
export interface StoredProject {
  id: number;
  owner: number;
  owner_name: string;
  name: string;
  kind: 'sequence' | 'song';
  duration: number;
  clips: number;
  revision: number;
  access: Access;
  created_at: string;
  updated_at: string;
  project: Project;
}

export type ProjectMeta = Omit<StoredProject, 'project'>;

/** What a preview request says about the mixer, so edits to mute/solo/volume need no re-upload. */
export interface TrackMix {
  id: string;
  mute: boolean;
  solo: boolean;
  volume: number;
}

export function clipLabel(c: ProjectClip): string {
  if (c.label) return c.label;
  const s = c.source;
  switch (s.type) {
    case 'corpus_clip':
      return s.word ?? `clip ${s.clip_id ?? ''}`.trim();
    case 'media':
      return s.asset;
    case 'synth':
      return s.notes.length === 1 ? `♪ ${s.notes[0].pitch}` : `♪ ${s.chip}`;
    case 'generated':
      return s.prompt || s.kind;
  }
}

export function projectDuration(p: Project): number {
  let end = 0;
  for (const t of p.tracks) for (const c of t.clips) end = Math.max(end, c.start + c.duration);
  return end;
}

/** The project's tracks as the timeline view draws them. */
export function toTimelineTracks(p: Project): Track[] {
  return p.tracks.map((t, i) => ({
    id: t.id,
    name: t.name || t.id,
    kind: t.kind === 'video' ? 'video' : t.generated ? 'midi' : 'audio',
    sub: t.generated ? 'from the song' : t.kind === 'video' ? 'picture' : 'audio',
    color: TRACK_COLORS[i % TRACK_COLORS.length],
    mute: t.mute,
    solo: t.solo,
    clips: t.clips.map((c) => ({ id: c.id, start: c.start, length: c.duration, label: clipLabel(c) }))
  }));
}

export function mixOf(p: Project): TrackMix[] {
  return p.tracks.map((t) => ({ id: t.id, mute: t.mute, solo: t.solo, volume: t.volume }));
}
