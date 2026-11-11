import { get, readable, writable, type Readable } from 'svelte/store';
import type { JobEvent, WsServerMessage } from './types';

export type WsStatus = 'idle' | 'connecting' | 'open' | 'reconnecting';

type Listener = (msg: WsServerMessage) => void;

const status = writable<WsStatus>('idle');
const jobs = writable<Record<string, JobEvent>>({});
const listeners = new Set<Listener>();
const topics = new Set<string>();

let socket: WebSocket | null = null;
let wanted = false;
let attempt = 0;
let retryTimer: ReturnType<typeof setTimeout> | undefined;
let pingTimer: ReturnType<typeof setInterval> | undefined;

function endpoint(): string {
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  return `${scheme}://${location.host}/api/ws`;
}

function send(msg: Record<string, unknown>): void {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg));
}

function handle(raw: string): void {
  let msg: WsServerMessage;
  try {
    msg = JSON.parse(raw) as WsServerMessage;
  } catch {
    return;
  }
  if (msg.type === 'job') {
    jobs.update((j) => ({ ...j, [msg.id]: { id: msg.id, state: msg.state, progress: msg.progress, message: msg.message } }));
  }
  for (const l of listeners) l(msg);
}

function open(): void {
  clearTimeout(retryTimer);
  status.set(attempt === 0 ? 'connecting' : 'reconnecting');
  const ws = new WebSocket(endpoint());
  socket = ws;
  ws.onopen = () => {
    attempt = 0;
    status.set('open');
    for (const t of topics) send({ type: 'subscribe', topic: t });
    clearInterval(pingTimer);
    pingTimer = setInterval(() => send({ type: 'ping' }), 25_000);
  };
  ws.onmessage = (ev) => {
    if (typeof ev.data === 'string') handle(ev.data);
  };
  ws.onclose = () => {
    if (socket === ws) socket = null;
    clearInterval(pingTimer);
    if (!wanted) {
      status.set('idle');
      return;
    }
    attempt += 1;
    status.set('reconnecting');
    const delay = Math.min(30_000, 500 * 2 ** Math.min(attempt, 6)) * (0.75 + Math.random() * 0.5);
    retryTimer = setTimeout(open, delay);
  };
  ws.onerror = () => ws.close();
}

/** Start (and keep alive) the /api/ws connection. Safe to call repeatedly. */
export function connectWs(): void {
  if (wanted) return;
  wanted = true;
  attempt = 0;
  open();
}

export function disconnectWs(): void {
  wanted = false;
  clearTimeout(retryTimer);
  clearInterval(pingTimer);
  socket?.close();
  socket = null;
  status.set('idle');
  jobs.set({});
}

/** Receive every server message. Returns an unsubscribe function. */
export function onWsMessage(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Ask the server for events on a topic such as `job:<id>`; re-sent after a reconnect. */
export function subscribeTopic(topic: string): void {
  topics.add(topic);
  send({ type: 'subscribe', topic });
}

export const wsStatus: Readable<WsStatus> = { subscribe: status.subscribe };
export const jobEvents: Readable<Record<string, JobEvent>> = { subscribe: jobs.subscribe };

/** Latest event for one job, as a store. */
export function jobStore(id: string): Readable<JobEvent | undefined> {
  return readable<JobEvent | undefined>(get(jobs)[id], (set) => jobs.subscribe((j) => set(j[id])));
}
