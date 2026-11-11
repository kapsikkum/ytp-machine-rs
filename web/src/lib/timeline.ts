export type TrackKind = 'video' | 'audio' | 'voice' | 'midi' | 'text';

export interface Clip {
  id: string;
  start: number;
  length: number;
  label: string;
}

export interface Note {
  /** seconds from the track start */
  t: number;
  d: number;
  /** 0..1, height in the lane */
  p: number;
}

export interface Track {
  id: string;
  name: string;
  kind: TrackKind;
  /** a sub-line under the name: instrument, voice, source */
  sub?: string;
  color: string;
  mute?: boolean;
  solo?: boolean;
  clips?: Clip[];
  notes?: Note[];
}

export const TRACK_COLORS = ['#ffe500', '#3ddc97', '#7dd3fc', '#ffb020', '#c4a1ff', '#ff5a5a'];

export function clock(sec: number): string {
  const s = Math.max(0, sec);
  const m = Math.floor(s / 60);
  const r = s - m * 60;
  return `${m}:${r.toFixed(2).padStart(5, '0')}`;
}
