<script lang="ts">
  import { api, errorText, NotImplementedError } from '#lib/api.ts';
  import { can } from '#lib/auth.ts';
  import { selectedVoice } from '#lib/voice.ts';
  import { sampleSong, toTracks, type Song } from '#lib/ytpmv/model.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import VoiceSelect from '#lib/components/VoiceSelect.svelte';
  import Timeline from '#lib/components/timeline/Timeline.svelte';
  import MidiImport from '#lib/components/ytpmv/MidiImport.svelte';
  import MidiLibrary from '#lib/components/ytpmv/MidiLibrary.svelte';
  import SongBar from '#lib/components/ytpmv/SongBar.svelte';
  import SoundDefaults from '#lib/components/ytpmv/SoundDefaults.svelte';
  import TransportBar from '#lib/components/ytpmv/TransportBar.svelte';
  import TrackInspector from '#lib/components/ytpmv/TrackInspector.svelte';
  import SongOptions, { type RenderOptions } from '#lib/components/ytpmv/SongOptions.svelte';
  import VideoPreview from '#lib/components/ytpmv/VideoPreview.svelte';

  let view = $state<'editor' | 'library'>('library');
  let tab = $state<'part' | 'song' | 'video'>('part');
  let song = $state<Song | null>(null);
  let selected = $state<string | null>(null);
  let playhead = $state(0);
  let playing = $state(false);

  let uploadStatus = $state('');
  let uploadError = $state(false);
  let status = $state('');
  let rendering = $state(false);

  let chip = $state('');
  let layout = $state('balanced');
  let fit = $state('contain');
  let center = $state(true);
  let o = $state<RenderOptions>({
    len: '0', speed: '1', transpose: '0', vary: 'off',
    balance: true, accurate: false, jitter: false,
    flip: true, flash: true, dim: true, labels: false, draft: false
  });

  const tracks = $derived(song ? toTracks(song, $selectedVoice) : []);
  const part = $derived(song?.parts.find((p) => p.id === selected));

  async function onfile(file: File) {
    uploadError = false;
    uploadStatus = `reading ${file.name}…`;
    try {
      await api.ytpmv.midi(file);
      uploadStatus = `${file.name} uploaded`;
    } catch (e) {
      uploadStatus = errorText(e);
      uploadError = !(e instanceof NotImplementedError);
    }
  }

  function openSample() {
    song = sampleSong();
    selected = song.parts[0].id;
    view = 'editor';
  }

  function flag(id: string, f: 'mute' | 'solo') {
    const p = song?.parts.find((x) => x.id === id);
    if (p) p[f] = !p[f];
  }

  function resetParts() {
    if (!song) return;
    const fresh = sampleSong();
    song.parts = fresh.parts;
  }

  const audition = () => (status = 'audio preview is not available yet');

  async function render() {
    if (!song) return;
    rendering = true;
    status = '';
    try {
      await api.ytpmv.render({ song: $state.snapshot(song), options: $state.snapshot(o), chip, layout, voice: $selectedVoice });
    } catch (e) {
      status = errorText(e);
    } finally {
      rendering = false;
    }
  }

  const tabs = [
    { id: 'part', label: 'Track' },
    { id: 'song', label: 'Song & render' },
    { id: 'video', label: 'Video preview' }
  ] as const;
</script>

<svelte:head><title>YTPMV · YTP Machine</title></svelte:head>

<TopBar>
  <VoiceSelect label="voice" title="Default voice" />
</TopBar>

<div class="ytpmv">
  <div class="project-tabs" role="tablist" aria-label="YTPMV views">
    <button role="tab" type="button" aria-selected={view === 'editor'} disabled={!song} onclick={() => (view = 'editor')}>Editor</button>
    <button role="tab" type="button" aria-selected={view === 'library'} onclick={() => (view = 'library')}>MIDI library</button>
  </div>

  {#if view === 'library'}
    <section class="view">
      <MidiImport status={uploadStatus} statusError={uploadError} {onfile} onsample={openSample} />
      <MidiLibrary />
    </section>
  {:else if song}
    <section class="view">
      <SongBar {song} canRender={$can('export.render')} {rendering} onimport={() => (view = 'library')} onrender={render} />
      <SoundDefaults bind:voice={$selectedVoice} bind:chip />
      <TransportBar bind:playhead bind:playing duration={song.duration} {status} onaudition={audition} />

      <div class="card parts-card">
        <Timeline {tracks} duration={song.duration} {selected} bind:playhead onselect={(id) => { selected = id; tab = 'part'; }} onflag={flag} empty="This song has no parts." />
        <div class="editor-tabs" role="tablist" aria-label="Workbench controls">
          {#each tabs as t (t.id)}
            <button role="tab" type="button" aria-selected={tab === t.id} onclick={() => (tab = t.id)}>{t.label}</button>
          {/each}
        </div>
        <div class="dock">
          {#if tab === 'part'}
            <TrackInspector part={part} defaultVoice={$selectedVoice} onplay={audition} />
          {:else if tab === 'song'}
            <SongOptions bind:o onresetparts={resetParts} />
          {:else}
            <VideoPreview parts={song.parts} bind:layout bind:fit bind:center />
          {/if}
        </div>
        <div class="footer"><span>{song.sample ? 'sample song — nothing is saved' : 'saved on this device'}</span></div>
      </div>
    </section>
  {/if}
</div>

<style>
  .ytpmv { width: min(1440px, 100% - 2rem); margin: 0 auto 3rem; }
  .project-tabs { display: flex; gap: 1rem; margin: 1rem 0 0; padding: 0 1rem; border-bottom: 1px solid var(--line); }
  .project-tabs button {
    padding: .7rem .2rem; border: 0; border-radius: 0; background: transparent; color: var(--muted); font-size: .82rem;
  }
  .project-tabs button:hover:not(:disabled) { background: transparent; color: var(--text); }
  .project-tabs button[aria-selected='true'] { color: var(--text); box-shadow: inset 0 -2px var(--yellow); }
  .view { display: flex; flex-direction: column; }
  .view > :global(.card:first-child) { margin-top: 1.25rem; }
  .parts-card { border-radius: 0; padding: 0; background: var(--surface); margin: 0; }
  .editor-tabs { display: flex; gap: 1rem; padding: .4rem .8rem 0; border-bottom: 1px solid var(--line-2); }
  .editor-tabs button { font-size: .75rem; border: 0; background: transparent; border-radius: 0; padding: .55rem .2rem; color: var(--muted); }
  .editor-tabs button:hover:not(:disabled) { background: transparent; color: var(--text); }
  .editor-tabs button[aria-selected='true'] { color: var(--text); box-shadow: inset 0 -2px var(--yellow); }
  .dock { min-height: 230px; }
  .footer { display: flex; padding: .55rem 1rem; border-top: 1px solid var(--line); font-size: .65rem; color: var(--muted); }
  @media (max-width: 650px) {
    .ytpmv { width: 100%; }
    .dock { min-height: 0; }
  }
</style>
