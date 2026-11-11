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

export interface JobEvent {
  id: string;
  state: JobState;
  progress: number;
  message?: string;
}
