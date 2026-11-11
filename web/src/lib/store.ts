import { writable, type Writable } from 'svelte/store';

// localStorage that never throws: private windows and blocked storage just
// forget, instead of taking the page down with them.
export const storage = {
  get<T>(key: string, fallback: T): T {
    try {
      const raw = localStorage.getItem(key);
      if (raw === null) return fallback;
      return (JSON.parse(raw) as T) ?? fallback;
    } catch {
      return fallback;
    }
  },
  set(key: string, value: unknown): void {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch {
      /* forget */
    }
  }
};

/** A writable store that is kept in localStorage. */
export function persisted<T>(key: string, fallback: T): Writable<T> {
  const s = writable<T>(storage.get(key, fallback));
  s.subscribe((v) => storage.set(key, v));
  return s;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`;
}

export function formatDate(iso: string | null | undefined): string {
  if (!iso) return '—';
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}
