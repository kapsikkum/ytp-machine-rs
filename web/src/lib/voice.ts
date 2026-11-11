import { get, writable } from 'svelte/store';
import { api, errorText } from './api';
import { can } from './auth';
import { persisted } from './store';
import { onWsMessage } from './ws';
import type { Corpus } from './types';

/** Installed corpora ("voices"), refreshed on demand and when the server says one changed. */
export const corpora = writable<Corpus[]>([]);
export const corporaError = writable<string>('');
export const corporaLoaded = writable(false);

/** Name of the corpus the user picked in the header. */
export const selectedVoice = persisted<string>('ytp-voice', '');

export async function loadCorpora(): Promise<void> {
  if (!get(can)('corpus.read')) {
    corpora.set([]);
    corporaLoaded.set(true);
    return;
  }
  try {
    const list = await api.corpora.list();
    corpora.set(list);
    corporaError.set('');
    if (!list.some((c) => c.name === get(selectedVoice))) selectedVoice.set(list[0]?.name ?? '');
  } catch (e) {
    corporaError.set(errorText(e));
  } finally {
    corporaLoaded.set(true);
  }
}

let stop: (() => void) | null = null;
/** Reload the list whenever the server announces a corpus change. Idempotent. */
export function watchCorpora(): void {
  if (stop) return;
  stop = onWsMessage((m) => {
    if (m.type === 'corpus') void loadCorpora();
  });
}
