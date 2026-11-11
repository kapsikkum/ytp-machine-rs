<script lang="ts">
  import { goto } from '$app/navigation';
  import { api, errorText, NotImplementedError } from '#lib/api.ts';
  import { can } from '#lib/auth.ts';
  import { persisted, storage } from '#lib/store.ts';
  import { corpora, selectedVoice } from '#lib/voice.ts';
  import { jobEvents, subscribeTopic } from '#lib/ws.ts';
  import { withDefaults, type Options } from '#lib/ytp/options.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import VoiceSelect from '#lib/components/VoiceSelect.svelte';
  import Composer from '#lib/components/ytp/Composer.svelte';
  import History, { type PastEntry } from '#lib/components/ytp/History.svelte';

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

  const progress = $derived.by<number | null>(() => {
    if (!busy) return null;
    const ev = jobId ? $jobEvents[jobId] : undefined;
    return ev ? ev.progress : -1;
  });

  // A job's websocket events drive the status line once a job exists.
  $effect(() => {
    const ev = jobId ? $jobEvents[jobId] : undefined;
    if (!ev) return;
    status = ev.message ?? ev.state;
    if (ev.state === 'done') {
      busy = false;
      status = 'done';
    } else if (ev.state === 'failed' || ev.state === 'cancelled') {
      busy = false;
      statusError = ev.state === 'failed';
    }
  });

  async function generate() {
    const said = text.trim();
    if (!said) return;
    busy = true;
    statusError = false;
    status = 'asking the server…';
    jobId = null;
    history.update((h) => [{ text: said, at: Date.now() }, ...h.filter((x) => x.text !== said)].slice(0, 100));
    try {
      const r = await api.ytp.generate({ text: said, voice: $selectedVoice || undefined, captions, options: opts });
      jobId = r.job_id;
      subscribeTopic(`job:${r.job_id}`);
      status = 'queued';
    } catch (e) {
      busy = false;
      status = errorText(e);
      // "not available yet" is a state of the server, not the user's mistake.
      statusError = !(e instanceof NotImplementedError);
    }
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
      {#if $can('studio.use')}
        <button type="button" class="ghost" onclick={openInStudio} title="Continue this sentence on the Studio timeline">Open in Studio</button>
      {/if}
    {/snippet}
  </Composer>

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
  footer { text-align: center; color: #3c3c43; font-size: .7rem; padding: 0 1rem 2rem; line-height: 1.9; }
  footer b { color: var(--dim); font-weight: 500; }
</style>
