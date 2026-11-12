// Mirrors docs/api.md. Keep in step with the contract.

export type Permission =
  | 'generate.ytp'
  | 'generate.ytpmv'
  | 'studio.use'
  | 'project.read'
  | 'project.write'
  | 'project.share'
  | 'export.render'
  | 'export.hq'
  | 'corpus.read'
  | 'corpus.edit'
  | 'corpus.install'
  | 'corpus.delete'
  | 'jobs.view_all'
  | 'jobs.cancel_any'
  | 'users.manage'
  | 'roles.manage'
  | 'tokens.create'
  | 'settings.manage'
  | 'audit.view';

export const ALL_PERMISSIONS: Permission[] = [
  'generate.ytp',
  'generate.ytpmv',
  'studio.use',
  'project.read',
  'project.write',
  'project.share',
  'export.render',
  'export.hq',
  'corpus.read',
  'corpus.edit',
  'corpus.install',
  'corpus.delete',
  'jobs.view_all',
  'jobs.cancel_any',
  'users.manage',
  'roles.manage',
  'tokens.create',
  'settings.manage',
  'audit.view'
];

export interface User {
  id: number;
  username: string;
  roles: string[];
  disabled: boolean;
  created_at: string;
}

export interface Quota {
  max_concurrent_jobs: number;
  max_render_seconds: number;
  max_height: number;
  requests_per_minute: number;
}

export interface Role {
  name: string;
  builtin: boolean;
  permissions: string[];
  quota: Quota;
}

export interface Token {
  id: number;
  name: string;
  permissions: string[];
  created_at: string;
  expires_at: string | null;
  last_used_at: string | null;
}

export interface AuditEntry {
  id: number;
  at: string;
  actor: string | null;
  action: string;
  target: string | null;
  detail: Record<string, unknown> | null;
  ip: string | null;
}

export interface Corpus {
  name: string;
  words: number;
  clips: number;
  sources: number;
  size_bytes: number;
  installed_at: string;
}

export interface AuthStateResponse {
  setup_required: boolean;
  user: User | null;
  permissions: string[];
  csrf: string | null;
}

export interface LoginResponse {
  user: User;
  permissions: string[];
  csrf: string;
}

export type JobState = 'queued' | 'running' | 'done' | 'failed' | 'cancelled';

export type WsServerMessage =
  | { type: 'hello'; user: User; permissions: string[] }
  | { type: 'job'; id: string; state: JobState; progress: number; message?: string }
  | { type: 'corpus'; event: 'installed' | 'removed'; name: string }
  | { type: 'error'; message: string }
  | { type: 'pong' };

export type WordStatus = 'found' | 'run' | 'spliced' | 'missing';

export interface JobToken {
  word: string;
  status: WordStatus;
  clips?: number[];
  approx?: boolean;
  stretched?: string;
  voice?: string;
  shown?: string;
}

export interface JobReport {
  found: string[];
  spliced: string[];
  missing: string[];
  runs: string[];
  tokens: JobToken[];
}

/** What `GET /api/jobs/:id` returns. */
export interface JobRecord {
  id: string;
  owner: number;
  owner_name: string;
  state: JobState;
  progress: number;
  message?: string;
  created_at: string;
  started_at?: string;
  finished_at?: string;
  text: string;
  voice: string;
  captions: boolean;
  options: Record<string, unknown>;
  report?: JobReport;
  timeline?: { i: number; start: number; end: number }[];
  duration?: number;
  size_bytes?: number;
  error?: string;
  warnings: string[];
  result_url?: string;
  /** `ytpmv` for a MIDI render; absent for a sentence. Then `report` is a {@link YtpmvReport}. */
  kind?: string;
}

// ── YTPMV ────────────────────────────────────────────────────────────────────

export interface MidiEntry {
  midi_id: string;
  title: string;
  duration: number;
  bpm: number;
  key: string;
  time_signature: string;
  tempo_changes: number;
  parts: number;
  drums: number;
  notes: number;
  description: string;
  bytes: number;
  added: number;
}

export interface AltTake {
  text: string;
  take: number;
  score: number;
}

export interface Recommended {
  text: string | null;
  take: number;
  takes?: number[];
  pool?: { text: string; take: number }[];
  octave: number;
  mode: string;
  score: number | null;
  alternatives: AltTake[];
}

export interface PartSettingsJson {
  id: string;
  corpus: string;
  text: string | null;
  take: number;
  takes: number[];
  octave: number;
  volume: number;
  pan: number;
  tone: string;
  program: number | null;
  mute: boolean;
  visible: boolean;
}

export interface PartAnalysis {
  id: string;
  name: string;
  role: string;
  instrument: string;
  is_drums: boolean;
  notes: number;
  range_midi: [number, number];
  recommended: Recommended;
  settings: PartSettingsJson;
  chip_tones: Record<string, string>;
  chip_tones_synth: Record<string, string>;
  /** [start, duration, pitch] */
  activity: [number, number, number][];
  program_table?: string;
}

/** [tone name, what it is] */
export type ToneChoice = [string, string];

export interface MidiAnalysis {
  midi_id: string;
  title: string;
  duration: number;
  bpm: number;
  key: string;
  time_signature: string;
  description: string;
  corpus: string;
  noises: Record<string, number>;
  units: Record<string, number>;
  instruments: {
    machines: Record<string, { name: string; melodic: ToneChoice[]; drums: ToneChoice[] }>;
    voice: ToneChoice[];
    synth: string[];
    programs: string[];
    chips: Record<string, string>;
  };
  parts: PartAnalysis[];
  program_tones: Record<string, Record<string, string[]>>;
}

/** The `X-Ytpmv-Info` header of a preview. */
export interface YtpmvInfo {
  from?: number;
  duration?: number;
  problems?: string[];
  parts?: { id: string; corpus: string; volume: number; pan: number }[];
}

/** The `report` of a finished `ytpmv` job. */
export interface YtpmvReport {
  duration: number;
  size: [number, number];
  problems: string[];
  options: Record<string, unknown>;
  parts: { id?: string; text?: string; tone?: string; corpus?: string }[];
}

export interface JobEvent {
  id: string;
  state: JobState;
  progress: number;
  message?: string;
}
