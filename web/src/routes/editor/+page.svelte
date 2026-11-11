<script lang="ts">
  import { onMount } from 'svelte';
  import { corpora, selectedVoice } from '#lib/voice.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import VoiceSelect from '#lib/components/VoiceSelect.svelte';
  import CorpusPack from '#lib/components/editor/CorpusPack.svelte';
  import { NOT_AVAILABLE } from '#lib/api.ts';

  // The corpus editor. The corpus word/clip endpoints are not in the contract
  // yet, so the waveform, transcript and edit form are the old layout with no
  // data behind them; install / delete of whole corpora is real (corpus pack).
  let gain = $state(1);
  let zoom = $state(1.5);
  let spectrogram = $state(false);
  let wStart = $state('');
  let wEnd = $state('');
  let wWord = $state('');
  let aWord = $state('');
  let status = $state('');
  let canvas = $state<HTMLCanvasElement>();

  const current = $derived($corpora.find((c) => c.name === $selectedVoice) ?? $corpora[0]);
  const counts = $derived(current ? `${current.clips.toLocaleString()} clips · ${current.words.toLocaleString()} words · ${current.sources.toLocaleString()} videos` : '');

  const unavailable = () => (status = NOT_AVAILABLE);

  function draw() {
    const c = canvas;
    if (!c) return;
    const dpr = window.devicePixelRatio || 1;
    c.width = c.clientWidth * dpr;
    c.height = c.clientHeight * dpr;
    const ctx = c.getContext('2d');
    if (!ctx) return;
    ctx.scale(dpr, dpr);
    const w = c.clientWidth, h = c.clientHeight;
    ctx.clearRect(0, 0, w, h);
    ctx.strokeStyle = '#25252a';
    ctx.beginPath();
    ctx.moveTo(0, h / 2);
    ctx.lineTo(w, h / 2);
    ctx.stroke();
    ctx.fillStyle = '#5a5a62';
    ctx.font = '12px "Roboto Mono", monospace';
    ctx.textAlign = 'center';
    ctx.fillText('no clip loaded — waveform not available yet', w / 2, h / 2 - 10);
  }

  onMount(() => {
    draw();
    const ro = new ResizeObserver(draw);
    if (canvas) ro.observe(canvas);
    return () => ro.disconnect();
  });
</script>

<svelte:head><title>corpus editor · YTP Machine</title></svelte:head>

<TopBar>
  <VoiceSelect title="Which voice to edit." />
</TopBar>
<div class="subbar">
  <span class="title">corpus editor</span>
  <select title="Which video" disabled><option>no videos loaded</option></select>
  <span class="muted">{counts}</span>
  <button type="button" class="ghost diag" onclick={unavailable} title="Check corpus health and view diagnostic metrics">health &amp; diagnostics</button>
  <span class="grow"></span>
  <CorpusPack />
</div>

<div class="page">
  <div class="layout">
    <div>
      <div class="panel" style="margin-bottom:1rem">
        <!-- svelte-ignore a11y_media_has_caption -->
        <video preload="metadata"></video>
        <div class="tools">
          <label>gain
            <input type="range" min="1" max="40" step="0.5" bind:value={gain} />
            <span class="val">{gain.toFixed(1)}x</span>
          </label>
          <label>zoom
            <input type="range" min="0.15" max="3" step="0.05" bind:value={zoom} />
            <span class="val">{zoom.toFixed(2)}s</span>
          </label>
          <label class="check"><input type="checkbox" bind:checked={spectrogram} /> spectrogram</label>
          <span class="grow2">
            <button type="button" onclick={unavailable}>▶ play</button>
            <button type="button" class="primary" onclick={unavailable}>save</button>
          </span>
        </div>
        <canvas bind:this={canvas}></canvas>
        <div class="hint">
          <b>Gain</b> amplifies quiet audio so you can see where a sound really starts and stops; <b>zoom</b> narrows the window around the word.
          <b>Drag the yellow edges</b> to move this word's boundaries, or <b>drag anywhere else</b> to mark out a new clip and name it on the right.
          <b>Ctrl+scroll</b> zooms. Grey lines are neighbouring words, green is the playhead.
        </div>
      </div>
      <div class="panel">
        <h2>Transcript — click a word to edit it</h2>
        <div class="words"><span class="hint">The transcript is not available yet.</span></div>
      </div>
    </div>

    <div class="panel side">
      <h2>Selected</h2>
      <div class="row"><label for="w-word">word</label><input id="w-word" bind:value={wWord} disabled /></div>
      <div class="row">
        <label for="w-start">start</label><input id="w-start" type="number" step="0.005" bind:value={wStart} disabled />
        <span class="nudge"><button disabled>−50</button><button disabled>−10</button><button disabled>+10</button><button disabled>+50</button></span>
      </div>
      <div class="row">
        <label for="w-end">end</label><input id="w-end" type="number" step="0.005" bind:value={wEnd} disabled />
        <span class="nudge"><button disabled>−50</button><button disabled>−10</button><button disabled>+10</button><button disabled>+50</button></span>
      </div>
      <div class="meta">Nothing selected — the waveform follows the video, so you can scrub to a gap and drag out a clip the transcription missed.</div>

      <h2 style="margin-top:1.2rem">Add a missing word</h2>
      <div class="row"><label for="a-word">word</label><input id="a-word" bind:value={aWord} placeholder="what was said" /></div>
      <div class="row"><span class="lab">span</span><span class="meta">drag on the waveform</span></div>
      <div class="row">
        <button type="button" style="flex:1" onclick={unavailable}>add word</button>
        <button type="button" style="flex:1" onclick={unavailable} title="A click, spew, hum or other non-verbal sound. Whisper cannot hear these at all.">add noise</button>
      </div>

      <div class="status" class:warn={status === NOT_AVAILABLE} role="status">{status}</div>
      <div class="hint" style="margin-top:.8rem">
        Edits apply immediately — the generator drops its cache on every change, so the next sentence uses the new timings. Nothing is written to the videos themselves, only to the corpus database.
      </div>
    </div>
  </div>
</div>

<style>
  .page { width: min(1400px, 100% - 2rem); margin: 1rem auto 3rem; font-size: 13px; }
  .layout { display: grid; grid-template-columns: minmax(0, 1fr) 23rem; gap: 1rem; align-items: start; }
  @media (max-width: 900px) { .layout { grid-template-columns: minmax(0, 1fr); } }
  .diag { padding: .28rem .65rem; font-size: .75rem; border-radius: 7px; color: var(--text); margin-left: .4rem; }

  video { width: 100%; max-height: 42vh; min-height: 8rem; background: #000; border-radius: 8px; display: block; }
  canvas { width: 100%; height: 130px; display: block; background: #0a0a0b; border: 1px solid var(--line); border-radius: 8px; cursor: crosshair; }
  .hint { margin-top: .5rem; }

  .tools { display: flex; gap: 1.1rem; align-items: center; flex-wrap: wrap; margin: .75rem 0 .5rem; }
  .tools label { color: var(--muted); font-size: .72rem; display: flex; gap: .5rem; align-items: center; }
  .tools input[type='range'] { width: 8.5rem; }
  .tools .check { cursor: pointer; gap: .35rem; }
  .val { color: var(--yellow); min-width: 2.8rem; text-align: right; font-variant-numeric: tabular-nums; }
  .grow2 { margin-left: auto; display: flex; gap: .4rem; }

  .words { display: flex; flex-wrap: wrap; gap: .3rem; max-height: 32vh; overflow-y: auto; padding: .1rem; }

  .row { display: flex; gap: .45rem; align-items: center; margin-bottom: .55rem; }
  .row label, .row .lab { color: var(--muted); width: 3.6rem; flex: none; font-size: .75rem; }
  .row input { flex: 1; min-width: 0; }
  .nudge { display: flex; gap: .25rem; }
  .nudge button { padding: .22rem .4rem; font-size: .7rem; border-radius: 6px; }
  .meta { color: var(--muted); line-height: 1.7; }
  .status { margin-top: .65rem; min-height: 1.2em; color: var(--muted); }
  .status.warn { color: var(--amber); }
</style>
