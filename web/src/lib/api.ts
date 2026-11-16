import type {
  AuditEntry,
  AuthStateResponse,
  BotsReport,
  Corpus,
  EncoderReport,
  JobRecord,
  LoginResponse,
  MediaAsset,
  MidiAnalysis,
  MidiEntry,
  PartSettingsJson,
  Preset,
  Recommended,
  Role,
  Quota,
  Share,
  Token,
  User,
  YtpmvInfo
} from './types';
import type { Project, ProjectMeta, StoredProject, TrackMix } from './project';

const BASE = '/api';

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

/** The server answered 501: the endpoint is part of a later phase. */
export class NotImplementedError extends ApiError {
  constructor(message = 'not available yet') {
    super(501, 'not_implemented', message);
    this.name = 'NotImplementedError';
  }
}

export const NOT_AVAILABLE = 'not available yet';

/** Human text for any thrown thing; 501 always reads "not available yet". */
export function errorText(e: unknown): string {
  if (e instanceof NotImplementedError) return NOT_AVAILABLE;
  if (e instanceof ApiError) return e.message || e.code;
  if (e instanceof Error) return e.message;
  return String(e);
}

let csrfToken: string | null = null;
let unauthorizedHandler: (() => void) | null = null;

export function setCsrf(token: string | null): void {
  csrfToken = token;
}

/** Called when a non-auth request comes back 401 (session expired). */
export function onUnauthorized(handler: (() => void) | null): void {
  unauthorizedHandler = handler;
}

type Query = Record<string, string | number | undefined | null>;

interface RequestOptions {
  method?: string;
  body?: unknown;
  form?: FormData;
  query?: Query;
  signal?: AbortSignal;
}

function url(path: string, query?: Query): string {
  let u = BASE + path;
  if (query) {
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(query)) if (v !== undefined && v !== null && v !== '') p.set(k, String(v));
    const s = p.toString();
    if (s) u += '?' + s;
  }
  return u;
}

async function request<T>(path: string, opts: RequestOptions = {}): Promise<T> {
  const method = opts.method ?? 'GET';
  const headers: Record<string, string> = { Accept: 'application/json' };
  let body: BodyInit | undefined;
  if (opts.form) {
    body = opts.form;
  } else if (opts.body !== undefined) {
    headers['Content-Type'] = 'application/json';
    body = JSON.stringify(opts.body);
  }
  if (method !== 'GET' && method !== 'HEAD' && csrfToken) headers['X-CSRF-Token'] = csrfToken;

  let res: Response;
  try {
    res = await fetch(url(path, opts.query), { method, headers, body, credentials: 'include', signal: opts.signal });
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e;
    throw new ApiError(0, 'network', `could not reach the server (${e instanceof Error ? e.message : String(e)})`);
  }

  if (res.status === 204) return undefined as T;

  const raw = await res.text();
  let data: unknown = undefined;
  if (raw) {
    try {
      data = JSON.parse(raw);
    } catch {
      data = undefined;
    }
  }

  if (!res.ok) {
    const obj = (data ?? {}) as { error?: string; message?: string };
    if (res.status === 501) throw new NotImplementedError(obj.message);
    if (res.status === 401 && !path.startsWith('/auth/')) unauthorizedHandler?.();
    const fallback = raw.trim() ? raw.trim().slice(0, 120) : res.statusText;
    throw new ApiError(res.status, obj.error ?? `http_${res.status}`, obj.message ?? `the server replied ${res.status} — ${fallback}`);
  }
  if (data === undefined && raw) throw new ApiError(res.status, 'bad_response', "the server replied with something that isn't JSON");
  return data as T;
}

/** POST JSON, get a WAV back, with what the server did in `X-Ytpmv-Info`. */
async function audio(path: string, body: unknown, signal?: AbortSignal): Promise<{ blob: Blob; info: YtpmvInfo }> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (csrfToken) headers['X-CSRF-Token'] = csrfToken;
  let res: Response;
  try {
    res = await fetch(url(path), { method: 'POST', headers, body: JSON.stringify(body), credentials: 'include', signal });
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e;
    throw new ApiError(0, 'network', `could not reach the server (${e instanceof Error ? e.message : String(e)})`);
  }
  if (!res.ok) {
    let obj: { error?: string; message?: string } = {};
    try {
      obj = (await res.json()) as typeof obj;
    } catch {
      /* not JSON */
    }
    if (res.status === 401) unauthorizedHandler?.();
    throw new ApiError(res.status, obj.error ?? `http_${res.status}`, obj.message ?? `the server replied ${res.status}`);
  }
  let info: YtpmvInfo = {};
  try {
    info = JSON.parse(res.headers.get('x-ytpmv-info') ?? '{}') as YtpmvInfo;
  } catch {
    /* no info */
  }
  return { blob: await res.blob(), info };
}

/** POST JSON, get a video back (the preview of a window), with what the server did in X-Preview-Info. */
async function video(path: string, body: unknown, signal?: AbortSignal): Promise<{ blob: Blob; info: PreviewInfo }> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (csrfToken) headers['X-CSRF-Token'] = csrfToken;
  let res: Response;
  try {
    res = await fetch(url(path), { method: 'POST', headers, body: JSON.stringify(body), credentials: 'include', signal });
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e;
    throw new ApiError(0, 'network', `could not reach the server (${e instanceof Error ? e.message : String(e)})`);
  }
  if (!res.ok) {
    let obj: { error?: string; message?: string } = {};
    try {
      obj = (await res.json()) as typeof obj;
    } catch {
      /* not JSON */
    }
    if (res.status === 401) unauthorizedHandler?.();
    throw new ApiError(res.status, obj.error ?? `http_${res.status}`, obj.message ?? `the server replied ${res.status}`);
  }
  let info: PreviewInfo = { cache: res.headers.get('x-preview-cache') ?? '', ms: Number(res.headers.get('x-preview-ms') ?? 0) };
  try {
    info = { ...info, ...(JSON.parse(res.headers.get('x-preview-info') ?? '{}') as object) };
  } catch {
    /* no info */
  }
  return { blob: await res.blob(), info };
}

export interface PreviewInfo {
  /** hit when the window came from the server's cache. */
  cache: string;
  /** Milliseconds the server took. */
  ms: number;
  kind?: string;
  clips?: number;
  proxied?: number;
}

export interface PreviewRequest {
  project_id?: number;
  project?: Project;
  start: number;
  duration: number;
  tracks?: Partial<TrackMix>[];
}

const json = (method: string, body?: unknown) => ({ method, body });
const enc = encodeURIComponent;

export const api = {
  health: () => request<{ ok: true; version: string }>('/health'),

  auth: {
    state: () => request<AuthStateResponse>('/auth/state'),
    setup: (token: string, username: string, password: string) =>
      request<{ user: User }>('/auth/setup', json('POST', { token, username, password })),
    login: (username: string, password: string) =>
      request<LoginResponse>('/auth/login', json('POST', { username, password })),
    logout: () => request<void>('/auth/logout', json('POST')),
    password: (current: string, next: string) =>
      request<void>('/auth/password', json('POST', { current, new: next }))
  },

  admin: {
    users: {
      list: () => request<User[]>('/admin/users'),
      create: (b: { username: string; password: string; roles: string[] }) =>
        request<User>('/admin/users', json('POST', b)),
      update: (id: number, b: { roles?: string[]; disabled?: boolean; password?: string }) =>
        request<User>(`/admin/users/${id}`, json('PATCH', b)),
      remove: (id: number) => request<void>(`/admin/users/${id}`, json('DELETE'))
    },
    roles: {
      list: () => request<Role[]>('/admin/roles'),
      create: (b: { name: string; permissions: string[]; quota?: Partial<Quota> }) =>
        request<Role>('/admin/roles', json('POST', b)),
      update: (name: string, b: { permissions?: string[]; quota?: Partial<Quota> }) =>
        request<Role>(`/admin/roles/${enc(name)}`, json('PATCH', b)),
      remove: (name: string) => request<void>(`/admin/roles/${enc(name)}`, json('DELETE'))
    },
    audit: (q: { limit?: number; before?: number } = {}) =>
      request<AuditEntry[]>('/admin/audit', { query: { limit: q.limit, before: q.before } }),
    encoders: () => request<EncoderReport>('/admin/encoders'),
    bots: () => request<BotsReport>('/admin/bots')
  },

  tokens: {
    list: () => request<Token[]>('/tokens'),
    create: (b: { name: string; permissions: string[]; expires_days?: number }) =>
      request<{ token: Token; secret: string }>('/tokens', json('POST', b)),
    remove: (id: number) => request<void>(`/tokens/${id}`, json('DELETE'))
  },

  corpora: {
    list: () => request<Corpus[]>('/corpora'),
    install: (file: File) => {
      const form = new FormData();
      form.append('file', file);
      return request<Corpus>('/corpora/install', { method: 'POST', form });
    },
    remove: (name: string) => request<void>(`/corpora/${enc(name)}`, json('DELETE'))
  },

  media: {
    list: () => request<MediaAsset[]>('/media'),
    upload: (file: File, name?: string) => {
      const form = new FormData();
      form.append('file', file);
      if (name?.trim()) form.append('name', name.trim());
      return request<MediaAsset>('/media', { method: 'POST', form });
    },
    remove: (id: string) => request<void>(`/media/${enc(id)}`, json('DELETE'))
  },

  ytp: {
    generate: (body: Record<string, unknown>) => request<{ job_id: string }>('/ytp/generate', json('POST', body)),
    /** The plan of a sentence as a project, not rendered; with save, stored and { id } answered. */
    plan: (body: Record<string, unknown>) => request<{ id?: number; project?: Project }>('/ytp/plan', json('POST', body))
  },
  ytpmv: {
    library: () => request<MidiEntry[]>('/ytpmv/library'),
    upload: (file: File, voice?: string) => {
      const form = new FormData();
      form.append('midi', file);
      if (voice) form.append('voice', voice);
      return request<MidiAnalysis>('/ytpmv/midi', { method: 'POST', form });
    },
    open: (id: string, voice?: string) => request<MidiAnalysis>(`/ytpmv/midi/${enc(id)}`, { query: { voice } }),
    rename: (id: string, title: string) => request<{ midi_id: string; title: string }>(`/ytpmv/midi/${enc(id)}`, json('PATCH', { title })),
    remove: (id: string) => request<void>(`/ytpmv/midi/${enc(id)}`, json('DELETE')),
    part: (id: string, part: string, voice?: string) =>
      request<{ id: string; recommended: Recommended; settings: PartSettingsJson }>(`/ytpmv/midi/${enc(id)}/part/${enc(part)}`, {
        query: { voice }
      }),
    sampleAudio: (body: { text: string; take?: number; corpus?: string; hit?: string }, signal?: AbortSignal) =>
      audio('/ytpmv/sample/audio', body, signal),
    previewPart: (body: Record<string, unknown>, signal?: AbortSignal) => audio('/ytpmv/preview', body, signal),
    previewMix: (body: Record<string, unknown>, signal?: AbortSignal) => audio('/ytpmv/preview-mix', body, signal),
    render: (body: Record<string, unknown>) => request<{ job_id: string }>('/ytpmv/render', json('POST', body)),
    plan: (body: Record<string, unknown>) => request<{ id?: number; project?: Project }>('/ytpmv/plan', json('POST', body))
  },
  jobs: {
    get: (id: string) => request<JobRecord>(`/jobs/${enc(id)}`),
    /** Cancels an active job; deletes a finished one with its video. */
    cancel: (id: string) => request<void>(`/jobs/${enc(id)}`, json('DELETE')),
    resultUrl: (id: string) => `${BASE}/jobs/${enc(id)}/result`
  },
  encode: {
    /** Every preset an export may ask for; `allowed: false` marks an hq preset this caller cannot use. */
    presets: () => request<{ hq: boolean; presets: Preset[] }>('/encode/presets'),
    /** Re-encode a finished job with another preset; a new `encode` job starts. */
    export: (id: string, preset: string) => request<{ job_id: string }>(`/jobs/${enc(id)}/export`, json('POST', { preset }))
  },
  projects: {
    list: () => request<ProjectMeta[]>('/projects'),
    get: (id: number) => request<StoredProject>(`/projects/${id}`),
    create: (project: Project, name?: string) => request<StoredProject>('/projects', json('POST', { project, name })),
    update: (id: number, b: { project?: Project; name?: string; revision?: number }) =>
      request<StoredProject>(`/projects/${id}`, json('PATCH', b)),
    remove: (id: number) => request<void>(`/projects/${id}`, json('DELETE')),
    shares: (id: number) => request<Share[]>(`/projects/${id}/shares`),
    share: (id: number, b: { username: string; access: 'read' | 'write' }) =>
      request<Share[]>(`/projects/${id}/shares`, json('POST', b)),
    unshare: (id: number, username: string) => request<void>(`/projects/${id}/shares/${enc(username)}`, json('DELETE')),
    render: (id: number) => request<{ job_id: string }>(`/projects/${id}/render`, json('POST'))
  },
  /** A low-res render of a window of a project. */
  preview: (body: PreviewRequest, signal?: AbortSignal) => video('/preview', body, signal)
};
