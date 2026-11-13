<script lang="ts">
  // Re-encodes a finished job with another preset. The export is a new
  // `encode` job, followed over the same websocket as every other job.
  import { onMount } from 'svelte';
  import { api, errorText } from '#lib/api.ts';
  import type { JobState, Preset } from '#lib/types.ts';
  import { jobEvents, subscribeTopic } from '#lib/ws.ts';

  let { job_id, onclose }: { job_id: string; onclose: () => void } = $props();

  let loading = $state(true);
  let presets = $state<Preset[]>([]);
  let selected = $state('');
  let loadError = $state('');

  let busy = $state(false);
  let error = $state('');
  let status = $state('');
  let progress = $state(-1);
  let exportId = $state<string | null>(null);
  let done = $state(false);

  const terminal = (s: JobState) => s === 'done' || s === 'failed' || s === 'cancelled';

  onMount(async () => {
    try {
      const r = await api.encode.presets();
      presets = r.presets;
      selected = r.presets.find((p) => p.allowed)?.name ?? '';
    } catch (e) {
      loadError = errorText(e);
    } finally {
      loading = false;
    }
  });

  // The record is the truth: it carries the result or the failure.
  async function refresh(id: string) {
    try {
      const j = await api.jobs.get(id);
      if (id !== exportId) return;
      if (!terminal(j.state)) return;
      busy = false;
      progress = 1;
      if (j.state === 'done') {
        done = true;
        status = 'done';
      } else {
        error = j.state === 'failed' ? (j.error ?? 'failed') : 'cancelled';
        status = '';
      }
    } catch (e) {
      if (id !== exportId) return;
      busy = false;
      status = '';
      error = errorText(e);
    }
  }

  // Websocket events drive the status line and the bar.
  $effect(() => {
    const id = exportId;
    const e = id ? $jobEvents[id] : undefined;
    if (!id || !e || !busy) return;
    if (!terminal(e.state)) {
      progress = e.progress;
      status = e.message ?? e.state;
    } else {
      void refresh(id);
    }
  });

  // If the socket is down nothing arrives, so look now and then as well.
  $effect(() => {
    const id = exportId;
    if (!id || !busy) return;
    const t = setInterval(() => void refresh(id), 2500);
    return () => clearInterval(t);
  });

  async function start() {
    if (!selected || busy) return;
    busy = true;
    error = '';
    done = false;
    status = 'starting…';
    progress = -1;
    exportId = null;
    try {
      const r = await api.encode.export(job_id, selected);
      exportId = r.job_id;
      subscribeTopic(`job:${r.job_id}`);
      status = 'queued';
      void refresh(r.job_id);
    } catch (e) {
      busy = false;
      status = '';
      error = errorText(e);
    }
  }

  function hint(p: Preset): string {
    const bits: string[] = [p.container.toUpperCase()];
    if (p.video_codec === 'none') {
      bits.push(`${p.audio_kbps} kbps audio`);
    } else {
      if (p.max_height >= 2160) bits.push('4K');
      else if (p.max_height) bits.push(`${p.max_height}p`);
      if (p.fps_cap) bits.push(`${p.fps_cap} fps`);
      if (p.target_mb !== null) bits.push(`~${p.target_mb} MB`);
      else if (p.crf !== null) bits.push(`CRF ${p.crf}`);
      else if (p.bitrate_kbps !== null) bits.push(`${p.bitrate_kbps} kbps`);
    }
    return bits.join(' · ');
  }
</script>

<div class="scrim" role="presentation">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="export-title">
    <div class="head">
      <h3 id="export-title">export again</h3>
      <span class="hint">re-encode the finished video with another preset</span>
    </div>

    {#if loading}
      <p class="hint">loading presets…</p>
    {:else if loadError}
      <p class="err">{loadError}</p>
    {:else}
      <fieldset class="presets">
        <legend class="section-label">preset</legend>
        {#each presets as p (p.name)}
          <label class="preset" class:on={selected === p.name} class:off={!p.allowed} title={p.allowed ? p.title : `${p.title} — needs export.hq`}>
            <input
              type="radio"
              name="export-preset"
              value={p.name}
              checked={selected === p.name}
              disabled={!p.allowed || busy}
              onchange={() => (selected = p.name)}
            />
            <span class="name">{p.title}</span>
            {#if p.hq}<span class="tag warn">hq</span>{/if}
            <span class="meta">{hint(p)}</span>
            {#if !p.allowed}<span class="meta">no permission</span>{/if}
          </label>
        {/each}
      </fieldset>
    {/if}

    {#if busy}
      <div class="progress"><div style:width="{Math.max(0, Math.round(progress * 100))}%"></div></div>
    {/if}

    {#if error}<p class="err" role="alert">{error}</p>{:else if status}<p class="hint" role="status">{status}</p>{/if}

    <div class="actions">
      {#if done && exportId}
        <a class="btn primary" href={api.jobs.resultUrl(exportId)} download>⤓ download</a>
        <button type="button" onclick={onclose}>close</button>
      {:else}
        <button type="button" onclick={onclose}>close</button>
        <button type="button" class="primary" disabled={loading || busy || !selected} onclick={start}>{busy ? 'exporting…' : 'export'}</button>
      {/if}
    </div>
  </div>
</div>

<style>
  .scrim { position: fixed; inset: 0; background: rgba(0, 0, 0, .7); z-index: 100; display: flex; align-items: center; justify-content: center; }
  .dialog {
    background: #18181b; border: 1px solid var(--line-2); border-radius: 12px; padding: 1.25rem;
    width: min(92vw, 560px); max-height: 90vh; overflow: auto;
    box-shadow: 0 20px 40px rgba(0, 0, 0, .6); display: flex; flex-direction: column; gap: .9rem;
  }
  .head { display: flex; align-items: baseline; justify-content: space-between; gap: .75rem; flex-wrap: wrap; }
  h3 { font-size: 1rem; font-weight: 600; color: var(--text); }
  .presets { border: 0; padding: 0; margin: 0; display: flex; flex-direction: column; gap: .3rem; }
  .presets legend { padding: 0; }
  .preset {
    display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center; gap: .2rem .55rem;
    padding: .45rem .6rem; border: 1px solid var(--line); border-radius: 8px; cursor: pointer;
    color: var(--muted); font-size: .8rem; transition: border-color .15s, background .15s;
  }
  .preset:hover:not(.off) { border-color: var(--line-2); background: #1f1f23; }
  .preset.on { border-color: var(--yellow); background: rgba(255, 229, 0, .06); color: var(--text); }
  .preset.off { opacity: .45; cursor: not-allowed; }
  .preset input { accent-color: var(--yellow); cursor: inherit; }
  .preset .name { color: var(--text); }
  .preset .meta { grid-column: 2 / -1; color: var(--dim); font-size: .7rem; }
  .progress { height: 3px; background: var(--line); border-radius: 999px; overflow: hidden; }
  .progress div { height: 100%; background: var(--yellow); border-radius: 999px; transition: width .35s var(--ease); }
  .actions { display: flex; justify-content: flex-end; gap: .5rem; flex-wrap: wrap; }
</style>
