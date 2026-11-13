<script lang="ts">
  import { goto } from '$app/navigation';
import { onDestroy, onMount, untrack } from 'svelte';
  import { api, ApiError, errorText } from '#lib/api.ts';
  import { can } from '#lib/auth.ts';
  import { storage } from '#lib/store.ts';
  import { selectedVoice } from '#lib/voice.ts';
  import { jobEvents, subscribeTopic } from '#lib/ws.ts';
  import type { JobRecord, JobState, MidiEntry, YtpmvReport } from '#lib/types.ts';
  import { fromAnalysis, serverParts, suggest, toTracks, type Part, type Song } from '#lib/ytpmv/model.ts';
  import * as player from '#lib/ytpmv/player.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import VoiceSelect from '#lib/components/VoiceSelect.svelte';
  import Timeline from '#lib/components/timeline/Timeline.svelte';
  import MidiImport from '#lib/components/ytpmv/MidiImport.svelte';
  import MidiLibrary from '#lib/components/ytpmv/MidiLibrary.svelte';
  import SimpleView from '#lib/components/ytpmv/SimpleView.svelte';
  import SongBar from '#lib/components/ytpmv/SongBar.svelte';
  import SoundDefaults from '#lib/components/ytpmv/SoundDefaults.svelte';
  import TransportBar from '#lib/components/ytpmv/TransportBar.svelte';
  import TrackInspector from '#lib/components/ytpmv/TrackInspector.svelte';
  import SongOptions, { type RenderOptions } from '#lib/components/ytpmv/SongOptions.svelte';
  import VideoPreview from '#lib/components/ytpmv/VideoPreview.svelte';
  import ExportDialog from '#lib/components/ExportDialog.svelte';

  let view = $state<'editor' | 'library'>('library');
  let mode = $state<'advanced' | 'simple'>(storage.get<string>('ytpmv-mode', 'advanced') === 'simple' ? 'simple' : 'advanced');
  let tab = $state<'part' | 'song' | 'video'>('part');
  let song = $state<Song | null>(null);
  let selected = $state<string | null>(null);
  let playhead = $state(0);

  $effect(() => storage.set('ytpmv-mode', mode));

  // The library
  let entries = $state<MidiEntry[]>([]);
  let libLoading = $state(true);
  let libError = $state('');
  let opening = $state<string | null>(null);
  let uploading = $state(false);
  let uploadStatus = $state('');
  let uploadError = $state(false);

  // Playing
  let busy = $state<'' | 'mix' | 'part' | 'word'>('');
  let playing = $state(false);
  let status = $state('');
  let statusError = $state(false);
  let playToken = 0;

  // Rendering
  let rendering = $state(false);
  let renderStatus = $state('');
  let renderError = $state(false);
  let jobId = $state<string | null>(null);
  let job = $state<JobRecord | null>(null);
  let exporting = $state(false);

  let chip = $state('');
  let layout = $state('balanced');
  let fit = $state('contain');
  let center = $state(true);
  let o = $state<RenderOptions>({
    len: '0', speed: '1', transpose: '0', vary: 'off',
    balance: true, accurate: false, jitter: false,
    flip: true, flash: true, dim: true, labels: false, draft: false, synth: false
  });

  const tracks = $derived(song ? toTracks(song, $selectedVoice, chip) : []);
  const part = $derived(song?.parts.find((p) => p.id === selected));
  const report = $derived(job?.kind === 'ytpmv' ? (job.report as unknown as YtpmvReport | undefined) : undefined);

  function renderOptions(): Record<string, unknown> {
    const len = Number(o.len);
    return {
      ...(len > 0 ? { max_seconds: len } : {}),
      speed: Number(o.speed),
      transpose: Number(o.transpose),
      vary: o.vary,
      balance: o.balance,
      accurate: o.accurate,
      jitter: o.jitter,
      flip: o.flip,
      flash: o.flash,
      dim: o.dim,
      labels: o.labels,
      draft: o.draft,
      synth: o.synth,
      ...(chip ? { chip } : {}),
      layout,
      media_fit: fit,
      center_tiles: center
    };
  }

  const fail = (e: unknown) => {
    if (e instanceof DOMException && e.name === 'AbortError') return '';
    return errorText(e);
  };

  // ── The library ────────────────────────────────────────────────────────────

  async function loadLibrary() {
    libLoading = true;
    try {
      entries = await api.ytpmv.library();
      libError = '';
    } catch (e) {
      libError = errorText(e);
    } finally {
      libLoading = false;
    }
  }

  onMount(() => void loadLibrary());
  onDestroy(() => {
    player.stop();
    clearInterval(poll);
  });

  function show(a: Awaited<ReturnType<typeof api.ytpmv.open>>) {
    player.stop();
    song = fromAnalysis(a);
    selected = song.parts[0]?.id ?? null;
    playhead = 0;
    job = null;
    jobId = null;
    exporting = false;
    renderStatus = '';
    status = '';
    view = 'editor';
  }

  async function onfile(file: File) {
    uploading = true;
    uploadError = false;
    uploadStatus = `reading ${file.name}…`;
    try {
      show(await api.ytpmv.upload(file, $selectedVoice || undefined));
      uploadStatus = '';
      void loadLibrary();
    } catch (e) {
      uploadStatus = errorText(e);
      uploadError = true;
    } finally {
      uploading = false;
    }
  }

  async function open(id: string) {
    opening = id;
    libError = '';
    try {
      show(await api.ytpmv.open(id, $selectedVoice || undefined));
    } catch (e) {
      libError = errorText(e);
      if (e instanceof ApiError && e.status === 404) void loadLibrary();
    } finally {
      opening = null;
    }
  }

  async function rename(id: string, title: string) {
    try {
      await api.ytpmv.rename(id, title);
      if (song?.midiId === id) song.title = title;
      libError = '';
      await loadLibrary();
    } catch (e) {
      libError = errorText(e);
    }
  }

  async function remove(id: string) {
    if (!confirm('Delete this MIDI file?')) return;
    try {
      await api.ytpmv.remove(id);
      if (song?.midiId === id) {
        song = null;
        selected = null;
      }
      libError = '';
      await loadLibrary();
    } catch (e) {
      libError = errorText(e);
    }
  }

  // The words are suggested for one voice; a new default voice means new suggestions.
  let suggestedFor = '';
  $effect(() => {
    const voice = $selectedVoice;
    untrack(() => {
      if (!song || !voice || song.analysis.corpus === voice || suggestedFor === voice + song.midiId) return;
      suggestedFor = voice + song.midiId;
      const id = song.midiId;
      status = `finding words in ${voice}…`;
      statusError = false;
      api.ytpmv
        .open(id, voice)
        .then((a) => {
          if (song?.midiId === id) {
            const keep = $state.snapshot(song.parts);
            const next = fromAnalysis(a);
            // Keep the choices made by hand: voices, instruments, levels, mutes.
            for (const p of next.parts) {
              const old = keep.find((x) => x.id === p.id);
              if (old) Object.assign(p, { corpus: old.corpus, tone: old.tone, mute: old.mute, solo: old.solo });
            }
            song = next;
            status = '';
          }
        })
        .catch((e) => {
          status = errorText(e);
          statusError = true;
        });
    });
  });

  // ── Listening ──────────────────────────────────────────────────────────────

  let ctl: AbortController | null = null;

  function stop() {
    ctl?.abort();
    ctl = null;
    playToken++;
    player.stop();
    playing = false;
    busy = '';
  }

  async function listen(what: 'mix' | 'part' | 'word', get: (signal: AbortSignal) => Promise<{ blob: Blob; info: { from?: number; problems?: string[] } }>, follow = false) {
    stop();
    const token = playToken;
    ctl = new AbortController();
    busy = what;
    status = what === 'word' ? '' : 'mixing…';
    statusError = false;
    try {
      const { blob, info } = await get(ctl.signal);
      if (token !== playToken) return;
      busy = '';
      playing = true;
      status = info.problems?.length ? info.problems[0] : '';
      const from = info.from ?? 0;
      await player.play(
        blob,
        follow ? (t) => (playhead = from + t) : undefined,
        () => {
          if (token === playToken) playing = false;
        }
      );
    } catch (e) {
      if (token !== playToken) return;
      busy = '';
      playing = false;
      status = fail(e);
      statusError = status !== '';
    }
  }

  function playMix(range: { from: number; length: number } | null) {
    if (!song) return;
    const s = song;
    const start = range ? range.from : playhead;
    void listen(
      'mix',
      (signal) =>
        api.ytpmv.previewMix(
          {
            midi_id: s.midiId,
            parts: serverParts(s),
            options: renderOptions(),
            voice: $selectedVoice || undefined,
            start,
            seconds: range ? range.length : null
          },
          signal
        ),
      true
    );
  }

  function playPart(p: Part) {
    if (!song) return;
    const s = song;
    void listen('part', (signal) =>
      api.ytpmv.previewMix(
        { midi_id: s.midiId, part_id: p.id, parts: serverParts(s), options: renderOptions(), voice: $selectedVoice || undefined, seconds: 10 },
        signal
      )
    );
  }

  function playWord(p: Part) {
    const text = p.text.trim();
    if (!text) return;
    void listen('word', (signal) =>
      api.ytpmv.sampleAudio({ text, take: p.take, corpus: p.corpus || $selectedVoice || undefined }, signal).then((r) => ({
        blob: r.blob,
        info: {}
      }))
    );
  }

  async function newVoice(p: Part) {
    if (!song) return;
    const id = song.midiId;
    const voice = p.corpus || $selectedVoice;
    if (!voice) return;
    try {
      const r = await api.ytpmv.part(id, p.id, voice);
      if (song?.midiId === id) suggest(p, { ...r.recommended, text: r.recommended.text, score: r.recommended.score }, r.settings.octave);
    } catch (e) {
      status = errorText(e);
      statusError = true;
    }
  }

  function flag(id: string, f: 'mute' | 'solo') {
    const p = song?.parts.find((x) => x.id === id);
    if (p) p[f] = !p[f];
  }

  function resetParts() {
    if (!song) return;
    song.parts = fromAnalysis(song.analysis).parts;
  }

  // ── Rendering ──────────────────────────────────────────────────────────────

  const terminal = (s: JobState) => s === 'done' || s === 'failed' || s === 'cancelled';
  let poll: ReturnType<typeof setInterval> | undefined;

  const progress = $derived.by<number | null>(() => {
    if (!rendering) return null;
    const ev = jobId ? $jobEvents[jobId] : undefined;
    return ev ? ev.progress : -1;
  });

  async function refresh(id: string) {
    try {
      const j = await api.jobs.get(id);
      if (id !== jobId) return;
      job = j;
      if (!terminal(j.state)) return;
      rendering = false;
      clearInterval(poll);
      renderError = j.state === 'failed';
      renderStatus = j.state === 'failed' ? (j.error ?? 'failed') : j.state === 'cancelled' ? 'cancelled' : 'done';
    } catch (e) {
      if (id !== jobId) return;
      if (e instanceof ApiError && [401, 403, 404].includes(e.status)) {
        rendering = false;
        clearInterval(poll);
        renderError = true;
        renderStatus = errorText(e);
      }
    }
  }

  $effect(() => {
    const id = jobId;
    const ev = id ? $jobEvents[id] : undefined;
    if (!id || !ev) return;
    if (!terminal(ev.state)) renderStatus = ev.message ?? ev.state;
    else if (untrack(() => job?.state) !== ev.state) void refresh(id);
  });

  // The finished render's plan as a project, opened in the Studio.
  let openingStudio = $state(false);
  async function openInStudio() {
    const j = job;
    if (!j || j.state !== 'done') return;
    openingStudio = true;
    try {
      const r = await api.ytpmv.plan({
        midi_id: j.options.midi_id,
        parts: j.options.parts,
        options: j.options.options,
        voice: j.voice || undefined,
        name: song?.title || undefined,
        save: true
      });
      await goto(`/?project=${r.id}`);
    } catch (e) {
      renderError = true;
      renderStatus = errorText(e);
    } finally {
      openingStudio = false;
    }
  }

  async function render() {
    if (!song) return;
    stop();
    rendering = true;
    renderError = false;
    renderStatus = 'asking the server…';
    job = null;
    jobId = null;
    exporting = false;
    clearInterval(poll);
    try {
      const r = await api.ytpmv.render({
        midi_id: song.midiId,
        parts: serverParts(song),
        options: renderOptions(),
        voice: $selectedVoice || undefined
      });
      jobId = r.job_id;
      subscribeTopic(`job:${r.job_id}`);
      renderStatus = 'queued';
      const id = r.job_id;
      poll = setInterval(() => void refresh(id), 2500);
      void refresh(id);
    } catch (e) {
      rendering = false;
      renderStatus = errorText(e);
      renderError = true;
    }
  }

  async function cancel() {
    const id = jobId;
    if (!id) return;
    renderStatus = 'cancelling…';
    try {
      await api.jobs.cancel(id);
    } catch (e) {
      renderError = true;
      renderStatus = errorText(e);
      return;
    }
    void refresh(id);
  }

  const tabs = [
    { id: 'part', label: 'Track' },
    { id: 'song', label: 'Song & render' },
    { id: 'video', label: 'Video preview' }
  ] as const;

  const summary = $derived.by(() => {
    if (!job || job.state !== 'done') return '';
    const bits: string[] = [];
    if (report) bits.push(`${report.duration.toFixed(1)} s`, `${report.size[0]}×${report.size[1]}`);
    if (job.size_bytes !== undefined) bits.push(`${(job.size_bytes / 1048576).toFixed(1)} MB`);
    return bits.join(' · ');
  });
</script>

<svelte:head><title>YTPMV · YTP Machine</title></svelte:head>

<TopBar>
  <VoiceSelect label="voice" title="Default voice" />
</TopBar>

<div class="ytpmv">
  <div class="project-tabs" role="tablist" aria-label="YTPMV views">
    <button role="tab" type="button" aria-selected={view === 'editor'} disabled={!song} onclick={() => (view = 'editor')}>Editor</button>
    <button role="tab" type="button" aria-selected={view === 'library'} onclick={() => (view = 'library')}>MIDI library</button>
    <span class="spacer"></span>
    <div class="modes" role="group" aria-label="YTPMV interface">
      <button type="button" aria-pressed={mode === 'simple'} onclick={() => (mode = 'simple')}>Simple</button>
      <button type="button" aria-pressed={mode === 'advanced'} onclick={() => (mode = 'advanced')}>Advanced</button>
    </div>
  </div>

  {#if view === 'library'}
    <section class="view">
      <MidiImport status={uploadStatus} statusError={uploadError} busy={uploading} {onfile} />
      <MidiLibrary {entries} loading={libLoading} error={libError} {opening} onopen={open} onrename={rename} ondelete={remove} />
    </section>
  {:else if song}
    <section class="view">
      <SongBar {song} canRender={$can('export.render')} {rendering} onimport={() => (view = 'library')} onrender={render} />
      <SoundDefaults bind:voice={$selectedVoice} bind:chip />
      {#if !$selectedVoice && !o.synth}
        <p class="note">No voice is installed, so there are no words to sing. Tick “chip only (no voice)” under Song &amp; render to hear this song on the chips.</p>
      {/if}
      <TransportBar bind:playhead duration={song.duration} {playing} loading={busy === 'mix'} {status} {statusError} onplay={playMix} onstop={stop} />

      {#if mode === 'advanced'}
        <div class="card parts-card">
          <Timeline {tracks} duration={song.duration} {selected} bind:playhead onselect={(id) => { selected = id; tab = 'part'; }} onflag={flag} empty="This song has no parts." />
          <div class="editor-tabs" role="tablist" aria-label="Workbench controls">
            {#each tabs as t (t.id)}
              <button role="tab" type="button" aria-selected={tab === t.id} onclick={() => (tab = t.id)}>{t.label}</button>
            {/each}
          </div>
          <div class="dock">
            {#if tab === 'part'}
              <TrackInspector
                part={part}
                analysis={song.analysis}
                defaultVoice={$selectedVoice}
                {chip}
                busy={busy === 'mix' ? '' : busy}
                onplaypart={() => part && playPart(part)}
                onplayword={() => part && playWord(part)}
                onvoice={() => part && newVoice(part)}
              />
            {:else if tab === 'song'}
              <SongOptions bind:o onresetparts={resetParts} />
            {:else}
              <VideoPreview parts={song.parts} bind:layout bind:fit bind:center />
            {/if}
          </div>
          <div class="footer"><span>{song.description}</span></div>
        </div>
      {:else}
        <div class="card parts-card">
          <SimpleView parts={song.parts} analysis={song.analysis} defaultVoice={$selectedVoice} busy={busy === 'mix' ? '' : busy} onplaypart={playPart} onplayword={playWord} onvoice={newVoice} />
          <SongOptions bind:o onresetparts={resetParts} />
          <VideoPreview parts={song.parts} bind:layout bind:fit bind:center />
        </div>
      {/if}

      {#if rendering || renderStatus}
        <div class="card render">
          <div class="line">
            <span class:err={renderError} role="status">{renderStatus}</span>
            {#if rendering && jobId}<button type="button" onclick={cancel}>Cancel</button>{/if}
          </div>
          {#if progress !== null}
            <div class="track"><div class="bar" class:indeterminate={progress < 0} style:width={progress >= 0 ? `${Math.round(progress * 100)}%` : undefined}></div></div>
          {/if}
        </div>
      {/if}

      {#if job && job.state === 'done' && job.result_url}
        {#key job.id}
          <div class="card result">
            <video src={job.result_url} controls playsinline preload="metadata"><track kind="captions" /></video>
            <div class="line">
              <span>{summary}</span>
              {#if job.id}<button type="button" class="btn" title="Re-encode this video with another preset" onclick={() => (exporting = true)}>⇥ export…</button>{/if}
              <a class="btn" href={job.result_url} download="{song.title || 'ytpmv'}.mp4">download</a>
        {#if $can('studio.use') && $can('project.write')}
          <button type="button" class="btn" disabled={openingStudio} onclick={openInStudio}>{openingStudio ? 'Opening…' : 'Open in Studio'}</button>
        {/if}
            </div>
            {#each report?.problems ?? [] as p, i (i)}<p class="warn">{p}</p>{/each}
            {#each job.warnings as w, i (i)}<p class="warn">{w}</p>{/each}
          </div>
        {/key}
      {/if}

      {#if exporting && jobId}
        <ExportDialog job_id={jobId} onclose={() => (exporting = false)} />
      {/if}
    </section>
  {/if}
</div>

<style>
  .ytpmv { width: min(1440px, 100% - 2rem); margin: 0 auto 3rem; }
  .project-tabs { display: flex; gap: 1rem; margin: 1rem 0 0; padding: 0 1rem; border-bottom: 1px solid var(--line); align-items: center; }
  .project-tabs > button {
    padding: .7rem .2rem; border: 0; border-radius: 0; background: transparent; color: var(--muted); font-size: .82rem;
  }
  .project-tabs > button:hover:not(:disabled) { background: transparent; color: var(--text); }
  .project-tabs > button[aria-selected='true'] { color: var(--text); box-shadow: inset 0 -2px var(--yellow); }
  .spacer { flex: 1; }
  .modes { display: flex; gap: 0; }
  .modes button { font-size: .72rem; min-height: 30px; border-radius: 0; }
  .modes button[aria-pressed='true'] { background: var(--yellow); color: #000; border-color: var(--yellow); }
  .view { display: flex; flex-direction: column; }
  .view > :global(.card:first-child) { margin-top: 1.25rem; }
  .note { margin: 0; padding: .5rem 1rem; font-size: .74rem; color: var(--amber); background: var(--surface); }
  .parts-card { border-radius: 0; padding: 0; background: var(--surface); margin: 0; }
  .editor-tabs { display: flex; gap: 1rem; padding: .4rem .8rem 0; border-bottom: 1px solid var(--line-2); }
  .editor-tabs button { font-size: .75rem; border: 0; background: transparent; border-radius: 0; padding: .55rem .2rem; color: var(--muted); }
  .editor-tabs button:hover:not(:disabled) { background: transparent; color: var(--text); }
  .editor-tabs button[aria-selected='true'] { color: var(--text); box-shadow: inset 0 -2px var(--yellow); }
  .dock { min-height: 230px; }
  .footer { display: flex; padding: .55rem 1rem; border-top: 1px solid var(--line); font-size: .65rem; color: var(--muted); }
  .render, .result { border-radius: 0; margin: 0; padding: .8rem 1rem; background: var(--surface); }
  .line { display: flex; align-items: center; gap: .8rem; font-size: .76rem; color: var(--muted); }
  .line span { flex: 1; }
  .track { height: 3px; background: var(--line); border-radius: 999px; overflow: hidden; margin-top: .6rem; }
  .bar { height: 100%; width: 0; background: var(--yellow); transition: width .35s var(--ease); }
  .bar.indeterminate { width: 30%; }
  .result video { width: 100%; max-height: 70vh; background: #000; margin-bottom: .6rem; }
  .warn { margin: .3rem 0 0; font-size: .72rem; color: var(--amber); }
  @media (max-width: 650px) {
    .ytpmv { width: 100%; }
    .dock { min-height: 0; }
  }
</style>
