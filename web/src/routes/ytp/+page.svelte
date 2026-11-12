<script lang="ts">
  import { goto } from '$app/navigation';
  import { api, ApiError, errorText, NotImplementedError } from '#lib/api.ts';
  import type { JobRecord, JobState } from '#lib/types.ts';
  import { can } from '#lib/auth.ts';
  import { persisted, storage } from '#lib/store.ts';
  import { corpora, selectedVoice } from '#lib/voice.ts';
  import { jobEvents, subscribeTopic } from '#lib/ws.ts';
  import { withDefaults, type Options } from '#lib/ytp/options.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import VoiceSelect from '#lib/components/VoiceSelect.svelte';
  import Composer from '#lib/components/ytp/Composer.svelte';
  import History, { type PastEntry } from '#lib/components/ytp/History.svelte';
  import ResultCard, { type ResultWord } from '#lib/components/ytp/ResultCard.svelte';

  let text = $state('');
  let opts = $state<Options>(withDefaults(storage.get<Options>('ytp-opts', {})));
  let captions = $state(storage.get('ytp-captions', true));
  let tweaksOpen = $state(storage.get('ytp-tweaks-open', false));
  const history = persisted<PastEntry[]>('ytp-history', []);

  $effect(() => storage.set('ytp-opts', opts));
  $effect(() => storage.set('ytp-captions', captions));
  $effect(() => storage.set('ytp-tweaks-open', tweaksOpen));

  let busy = $state(false);
  let status = $state('');
  let statusError = $state(false);
  let jobId = $state<string | null>(null);
  let job = $state<JobRecord | null>(null);

  const progress = $derived.by<number | null>(() => {
    if (!busy) return null;
    const ev = jobId ? $jobEvents[jobId] : undefined;
    return ev ? ev.progress : -1;
  });

  const terminal = (s: JobState) => s === 'done' || s === 'failed' || s === 'cancelled';

  // The record is the truth: it carries the result, the error and the warnings.
  // Called for the first look (the job may already be over before the socket
  // subscribes), on every terminal websocket event, and by the poll below.
  async function refresh(id: string) {
    try {
      const j = await api.jobs.get(id);
      if (id !== jobId) return;
      job = j;
      if (!terminal(j.state)) return;
      busy = false;
      statusError = j.state === 'failed';
      status = j.state === 'failed' ? (j.error ?? 'failed') : j.state === 'cancelled' ? 'cancelled' : 'done';
    } catch (e) {
      if (id !== jobId) return;
      // Someone deleted it, or the session ended: nothing more will come.
      if (e instanceof ApiError && (e.status === 404 || e.status === 401 || e.status === 403)) {
        busy = false;
        statusError = true;
        status = errorText(e);
      }
    }
  }

  // Websocket events drive the status line while the job runs.
  $effect(() => {
    const id = jobId;
    const ev = id ? $jobEvents[id] : undefined;
    if (!id || !ev) return;
    if (!terminal(ev.state)) {
      status = ev.message ?? ev.state;
    } else if (job?.state !== ev.state) {
      void refresh(id);
    }
  });

  // If the socket is down nothing arrives, so look now and then as well.
  $effect(() => {
    const id = jobId;
    if (!id || !busy) return;
    const t = setInterval(() => void refresh(id), 2500);
    return () => clearInterval(t);
  });

  const words = $derived.by<ResultWord[]>(() => {
    const r = job?.report;
    if (!r) return [];
    const at = new Map((job?.timeline ?? []).map((s) => [s.i, s.start]));
    return r.tokens.map((t, i) => ({ word: t.shown ?? t.word, kind: t.status, start: at.get(i) }));
  });

  const summary = $derived.by(() => {
    if (!job) return '';
    const r = job.report;
    const parts: string[] = [];
    if (job.duration !== undefined) parts.push(`${job.duration.toFixed(1)} s`);
    if (r) {
      const n = (s: string) => r.tokens.filter((t) => t.status === s).length;
      parts.push(`${n('found') + n('run')} found`);
      if (n('spliced')) parts.push(`${n('spliced')} spliced`);
      if (n('missing')) parts.push(`${n('missing')} missing`);
    }
    if (job.size_bytes !== undefined) parts.push(`${(job.size_bytes / 1048576).toFixed(1)} MB`);
    return parts.join(' · ');
  });

  async function generate() {
    const said = text.trim();
    if (!said) return;
    busy = true;
    statusError = false;
    status = 'asking the server…';
    jobId = null;
    job = null;
    history.update((h) => [{ text: said, at: Date.now() }, ...h.filter((x) => x.text !== said)].slice(0, 100));
    try {
      const r = await api.ytp.generate({ text: said, voice: $selectedVoice || undefined, captions, options: opts });
      jobId = r.job_id;
      subscribeTopic(`job:${r.job_id}`);
      status = 'queued';
      void refresh(r.job_id);
    } catch (e) {
      busy = false;
      status = errorText(e);
      // "not available yet" is a state of the server, not the user's mistake.
      statusError = !(e instanceof NotImplementedError);
    }
  }

  async function cancel() {
    const id = jobId;
    if (!id) return;
    status = 'cancelling…';
    try {
      await api.jobs.cancel(id);
    } catch (e) {
      statusError = true;
      status = errorText(e);
      return;
    }
    void refresh(id);
  }

  // Same words, picked again: no seed is sent, so the server picks a new one.
  function again() {
    if (busy) return;
    text = job?.text ?? text;
    void generate();
  }

  function openInStudio() {
    const q = new URLSearchParams({ mode: 'ytp', prompt: text });
    void goto('/?' + q.toString());
  }

  const stats = $derived.by(() => {
    const c = $corpora.find((x) => x.name === $selectedVoice) ?? $corpora[0];
    return c ? { words: c.words, clips: c.clips, sources: c.sources } : null;
  });
</script>

<svelte:head><title>say · YTP Machine</title></svelte:head>

<TopBar>
  <VoiceSelect title="Which voice (corpus) says it" />
</TopBar>

<main>
  <Composer bind:text bind:opts bind:captions bind:tweaksOpen {busy} {status} {statusError} {progress} onsubmit={generate}>
    {#snippet actions()}
      {#if busy && jobId}
        <button type="button" class="ghost" onclick={cancel} title="Stop this job">Cancel</button>
      {/if}
      {#if $can('studio.use')}
        <button type="button" class="ghost" onclick={openInStudio} title="Continue this sentence on the Studio timeline">Open in Studio</button>
      {/if}
    {/snippet}
  </Composer>

  {#if job && job.warnings.length}
    <ul class="warnings">
      {#each job.warnings as w, i (i)}<li>{w}</li>{/each}
    </ul>
  {/if}

  {#if job && job.state === 'done' && job.result_url}
    {#key job.id}
      <ResultCard src={job.result_url} {words} {summary} onagain={again} />
    {/key}
  {/if}

  <History items={$history} onuse={(t) => (text = t)} onclear={() => history.set([])} />
</main>

<footer>
  {#if stats}
    <b>{stats.clips.toLocaleString()}</b> clips · <b>{stats.words.toLocaleString()}</b> words · <b>{stats.sources.toLocaleString()}</b> videos
  {/if}
</footer>

<style>
  main {
    width: min(760px, 100% - 2rem);
    margin: clamp(1.25rem, 5vh, 3rem) auto 3rem;
    display: flex; flex-direction: column; gap: 1rem;
  }
  .warnings { margin: 0; padding: 0 .5rem; list-style: none; color: var(--amber); font-size: .74rem; display: flex; flex-direction: column; gap: .2rem; }
  footer { text-align: center; color: #3c3c43; font-size: .7rem; padding: 0 1rem 2rem; line-height: 1.9; }
  footer b { color: var(--dim); font-weight: 500; }
</style>
