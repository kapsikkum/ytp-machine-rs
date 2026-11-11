<script lang="ts">
  import { page } from '$app/state';
  import { api, errorText, NotImplementedError } from '#lib/api.ts';
  import { can } from '#lib/auth.ts';
  import { TRACK_COLORS, type Track, type TrackKind } from '#lib/timeline.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import VoiceSelect from '#lib/components/VoiceSelect.svelte';
  import PreviewPane from '#lib/components/studio/PreviewPane.svelte';
  import PromptBox, { type Mode } from '#lib/components/studio/PromptBox.svelte';
  import Timeline from '#lib/components/timeline/Timeline.svelte';

  // The Studio: preview, prompt, and a multi-track timeline. The project API
  // (/api/projects/*) and generation endpoints answer 501 for now, so this is
  // the layout and the plumbing, with an empty project.
  const q = page.url.searchParams;
  let mode = $state<Mode>(q.get('mode') === 'ytpmv' ? 'ytpmv' : 'ytp');
  let prompt = $state(q.get('prompt') ?? '');

  let title = $state('Untitled project');
  let playhead = $state(0);
  let playing = $state(false);
  let selected = $state<string | null>(null);
  let note = $state('');
  let noteIsError = $state(false);

  let tracks = $state<Track[]>([
    { id: 'v1', name: 'Video 1', kind: 'video', sub: 'picture', color: TRACK_COLORS[0], clips: [] },
    { id: 'a1', name: 'Voice 1', kind: 'voice', sub: 'corpus voice', color: TRACK_COLORS[1], clips: [] },
    { id: 'a2', name: 'Music 1', kind: 'audio', sub: 'audio', color: TRACK_COLORS[2], clips: [] }
  ]);

  const duration = $derived(Math.max(30, ...tracks.flatMap((t) => (t.clips ?? []).map((c) => c.start + c.length))));

  let counter = 3;
  function addTrack(kind: TrackKind) {
    counter += 1;
    const label = { video: 'Video', voice: 'Voice', audio: 'Music', midi: 'MIDI', text: 'Text' }[kind];
    const n = tracks.filter((t) => t.kind === kind).length + 1;
    const id = `t${counter}`;
    tracks.push({ id, name: `${label} ${n}`, kind, color: TRACK_COLORS[(counter - 1) % TRACK_COLORS.length], clips: [] });
    selected = id;
  }

  function flag(id: string, f: 'mute' | 'solo') {
    const t = tracks.find((x) => x.id === id);
    if (t) t[f] = !t[f];
  }

  function removeSelected() {
    if (selected) tracks = tracks.filter((t) => t.id !== selected);
    selected = null;
  }

  async function saveProject() {
    note = '';
    noteIsError = false;
    try {
      await api.projects.list();
      note = 'project saving is not wired up yet';
    } catch (e) {
      note = errorText(e);
      noteIsError = !(e instanceof NotImplementedError);
    }
  }

  async function exportVideo() {
    note = '';
    noteIsError = false;
    try {
      await api.ytpmv.render({ tracks: $state.snapshot(tracks) });
    } catch (e) {
      note = errorText(e);
      noteIsError = !(e instanceof NotImplementedError);
    }
  }
</script>

<svelte:head><title>studio · YTP Machine</title></svelte:head>

<TopBar>
  <VoiceSelect title="The default voice for new tracks" />
</TopBar>

<div class="subbar">
  <span class="title">studio</span>
  <input class="name" type="text" bind:value={title} aria-label="Project name" />
  <span class="grow"></span>
  {#if note}<span class="note" class:err={noteIsError}>{note}</span>{/if}
  {#if $can('project.write')}<button type="button" onclick={saveProject}>save</button>{/if}
  {#if $can('export.render')}<button type="button" class="primary" onclick={exportVideo}>render video</button>{/if}
</div>

<div class="studio">
  <div class="upper">
    <PreviewPane bind:playhead bind:playing {duration} />
    <PromptBox bind:mode bind:prompt />
  </div>

  <div class="tl">
    <div class="tl-tools">
      <span class="section-label">timeline</span>
      <span class="add">
        add track
        <button type="button" onclick={() => addTrack('video')}>+ video</button>
        <button type="button" onclick={() => addTrack('voice')}>+ voice</button>
        <button type="button" onclick={() => addTrack('audio')}>+ music</button>
        <button type="button" onclick={() => addTrack('midi')}>+ midi</button>
      </span>
      <span class="grow"></span>
      <button type="button" class="danger" disabled={!selected} onclick={removeSelected}>remove track</button>
    </div>
    <Timeline {tracks} {duration} {selected} bind:playhead onselect={(id) => (selected = id)} onflag={flag} empty="Add a track to begin." />
  </div>
</div>

<style>
  .subbar .name { width: min(22rem, 100%); font-size: .8rem; }
  .note { color: var(--amber); font-size: .74rem; }
  .note.err { color: var(--red); }
  .studio { width: min(1440px, 100% - 2rem); margin: 1rem auto 3rem; display: flex; flex-direction: column; gap: 1rem; }
  .upper { display: grid; grid-template-columns: minmax(0, 1.5fr) minmax(280px, 1fr); gap: 1rem; align-items: start; }
  @media (max-width: 900px) { .upper { grid-template-columns: minmax(0, 1fr); } }
  .tl-tools { display: flex; align-items: center; gap: .6rem 1rem; flex-wrap: wrap; margin-bottom: .6rem; }
  .tl-tools .section-label { margin: 0; }
  .add { display: inline-flex; align-items: center; gap: .35rem; color: var(--dim); font-size: .72rem; }
  .add button { font-size: .72rem; padding: .22rem .55rem; }
  .grow { flex: 1; }
</style>
