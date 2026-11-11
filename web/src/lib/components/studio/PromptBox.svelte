<script lang="ts" module>
  export type Mode = 'ytp' | 'ytpmv';
</script>

<script lang="ts">
  import { api, errorText, NotImplementedError } from '#lib/api.ts';
  import { can } from '#lib/auth.ts';
  import { selectedVoice } from '#lib/voice.ts';
  import { jobEvents, subscribeTopic } from '#lib/ws.ts';

  let { mode = $bindable('ytp'), prompt = $bindable('') }: { mode?: Mode; prompt?: string } = $props();

  let busy = $state(false);
  let status = $state('');
  let notice = $state(false); // "not available yet" rather than a real failure
  let failed = $state(false);
  let jobId = $state<string | null>(null);

  const allowedModes = $derived((['ytp', 'ytpmv'] as Mode[]).filter((m) => $can(m === 'ytp' ? 'generate.ytp' : 'generate.ytpmv')));
  const ev = $derived(jobId ? $jobEvents[jobId] : undefined);

  $effect(() => {
    if (!ev) return;
    status = ev.message ?? ev.state;
    failed = ev.state === 'failed';
    if (ev.state === 'done' || ev.state === 'failed' || ev.state === 'cancelled') busy = false;
  });

  async function go() {
    if (!prompt.trim() || busy) return;
    busy = true;
    notice = false;
    failed = false;
    status = 'asking the server…';
    jobId = null;
    try {
      const r =
        mode === 'ytp'
          ? await api.ytp.generate({ text: prompt.trim(), voice: $selectedVoice || undefined })
          : await api.ytpmv.render({ prompt: prompt.trim(), voice: $selectedVoice || undefined });
      jobId = r.job_id;
      subscribeTopic(`job:${r.job_id}`);
      status = 'queued';
    } catch (e) {
      busy = false;
      status = errorText(e);
      notice = e instanceof NotImplementedError;
      failed = !notice;
    }
  }

  function keydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void go();
    }
  }
</script>

<section class="card prompt">
  <div class="top-row">
    <div class="section-label">prompt</div>
    <div class="seg" role="group" aria-label="Kind of video">
      {#each allowedModes as m (m)}
        <button type="button" class:on={mode === m} aria-pressed={mode === m} onclick={() => (mode = m)}>{m === 'ytp' ? 'YTP' : 'YTPMV'}</button>
      {/each}
    </div>
  </div>
  <textarea
    rows="4"
    spellcheck="false"
    aria-label="Prompt"
    placeholder={mode === 'ytp' ? 'type something for the voice to say…' : 'describe the song, or pick a MIDI in the YTPMV page…'}
    bind:value={prompt}
    onkeydown={keydown}
  ></textarea>
  <div class="bar">
    <span class="status" class:warn={notice} class:error={failed} role="status">{status}</span>
    <button type="button" class="go" disabled={busy || !prompt.trim() || allowedModes.length === 0} onclick={go}>
      {mode === 'ytp' ? 'SAY IT' : 'MAKE IT'}<kbd>Ctrl ↵</kbd>
    </button>
  </div>
  {#if ev && busy}
    <div class="progress"><div style:width="{Math.round(ev.progress * 100)}%"></div></div>
  {/if}
</section>

<style>
  .prompt { padding: 1rem; display: flex; flex-direction: column; gap: .7rem; min-width: 0; }
  .top-row { display: flex; align-items: center; justify-content: space-between; gap: .6rem; }
  .top-row .section-label { margin: 0; }
  .seg { display: inline-flex; padding: 2px; border: 1px solid var(--line-2); border-radius: 999px; }
  .seg button { border: 0; background: transparent; border-radius: 999px; padding: .28rem .8rem; font-size: .72rem; color: var(--muted); }
  .seg button:hover:not(:disabled) { background: transparent; color: var(--text); }
  .seg button.on { background: var(--yellow); color: #000; }
  textarea { width: 100%; resize: vertical; min-height: 6rem; font-size: 1rem; line-height: 1.45; padding: .6rem .7rem; caret-color: var(--yellow); }
  .bar { display: flex; align-items: center; justify-content: space-between; gap: .75rem; flex-wrap: wrap; border-top: 1px solid var(--line); padding-top: .7rem; }
  .status { font-size: .76rem; color: var(--muted); min-height: 1.1em; flex: 1; }
  .status.warn { color: var(--amber); }
  .go {
    display: inline-flex; align-items: center; cursor: pointer; background: var(--yellow); color: #000; border: none; border-radius: 999px;
    font-family: Impact, 'Arial Black', sans-serif; font-size: 1.05rem; letter-spacing: 1.5px; padding: .5rem 1.3rem .45rem;
    box-shadow: 0 6px 20px rgba(255, 229, 0, .18);
  }
  .go:hover:not(:disabled) { background: var(--yellow-2); border-color: transparent; }
  .go:disabled { opacity: .35; cursor: not-allowed; box-shadow: none; }
  .progress { height: 3px; background: var(--line); border-radius: 999px; overflow: hidden; }
  .progress div { height: 100%; background: var(--yellow); border-radius: 999px; transition: width .35s var(--ease); }
</style>
