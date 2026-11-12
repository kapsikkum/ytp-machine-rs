<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { api, ApiError, errorText, type PreviewInfo } from '#lib/api.ts';
  import { can } from '#lib/auth.ts';
  import { mixOf, projectDuration, toTimelineTracks, type Project, type ProjectMeta, type StoredProject } from '#lib/project.ts';
  import { TRACK_COLORS, type Track, type TrackKind } from '#lib/timeline.ts';
  import { jobEvents, subscribeTopic } from '#lib/ws.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import VoiceSelect from '#lib/components/VoiceSelect.svelte';
  import PreviewPane from '#lib/components/studio/PreviewPane.svelte';
  import PromptBox, { type Mode } from '#lib/components/studio/PromptBox.svelte';
  import Timeline from '#lib/components/timeline/Timeline.svelte';

  // The Studio: preview, prompt, and a multi-track timeline. `?project=<id>`
  // opens a project (what "Open in Studio" on the Say and Song pages makes):
  // its tracks and clips are drawn by time, and the preview plays the window
  // under the playhead, rendered on the server. Editing comes next; for now a
  // project is read-only apart from the mute/solo switches, which change what
  // the preview mixes and can be saved.
  const q = page.url.searchParams;
  let mode = $state<Mode>(q.get('mode') === 'ytpmv' ? 'ytpmv' : 'ytp');
  let prompt = $state(q.get('prompt') ?? '');

  const projectId = $derived.by(() => {
    const n = Number(page.url.searchParams.get('project'));
    return Number.isInteger(n) && n > 0 ? n : null;
  });

  let stored = $state<StoredProject | null>(null);
  let project = $state<Project | null>(null);
  let dirty = $state(false);
  let loading = $state(false);
  let loadError = $state('');
  let recent = $state<ProjectMeta[]>([]);

  let playhead = $state(0);
  let playing = $state(false);
  let selected = $state<string | null>(null);
  let note = $state('');
  let noteIsError = $state(false);
  let lastPreview = $state('');

  // Load the project named in the address; list the recent ones when none is.
  $effect(() => {
    const id = projectId;
    stored = null;
    project = null;
    dirty = false;
    loadError = '';
    playhead = 0;
    playing = false;
    note = '';
    if (id === null) {
      if ($can('project.read')) api.projects.list().then((l) => (recent = l)).catch(() => (recent = []));
      return;
    }
    loading = true;
    let live = true;
    api.projects
      .get(id)
      .then((r) => {
        if (!live) return;
        stored = r;
        project = r.project;
      })
      .catch((e) => {
        if (live) loadError = e instanceof ApiError && e.status === 404 ? 'There is no such project, or it is not shared with you.' : errorText(e);
      })
      .finally(() => {
        if (live) loading = false;
      });
    return () => {
      live = false;
    };
  });

  // Local tracks of an empty Studio (no project open): only the layout.
  let localTracks = $state<Track[]>([
    { id: 'v1', name: 'Video 1', kind: 'video', sub: 'picture', color: TRACK_COLORS[0], clips: [] },
    { id: 'a1', name: 'Voice 1', kind: 'voice', sub: 'corpus voice', color: TRACK_COLORS[1], clips: [] },
    { id: 'a2', name: 'Music 1', kind: 'audio', sub: 'audio', color: TRACK_COLORS[2], clips: [] }
  ]);
  let counter = 3;
  function addTrack(kind: TrackKind) {
    counter += 1;
    const label = { video: 'Video', voice: 'Voice', audio: 'Music', midi: 'MIDI', text: 'Text' }[kind];
    const n = localTracks.filter((t) => t.kind === kind).length + 1;
    const id = `t${counter}`;
    localTracks.push({ id, name: `${label} ${n}`, kind, color: TRACK_COLORS[(counter - 1) % TRACK_COLORS.length], clips: [] });
    selected = id;
  }
  function removeSelected() {
    if (selected) localTracks = localTracks.filter((t) => t.id !== selected);
    selected = null;
  }

  const tracks = $derived<Track[]>(project ? toTimelineTracks(project) : localTracks);
  const duration = $derived(project ? Math.max(1, projectDuration(project) + (stored?.kind === 'song' ? 0.8 : 0)) : 30);
  const canWrite = $derived(!!stored && stored.access !== 'read' && $can('project.write'));

  function flag(id: string, f: 'mute' | 'solo') {
    if (!project) {
      const t = localTracks.find((x) => x.id === id);
      if (t) t[f] = !t[f];
      return;
    }
    const t = project.tracks.find((x) => x.id === id);
    if (t) {
      t[f] = !t[f];
      dirty = true;
    }
  }

  // What the preview windows are made of: changes when the project, its
  // revision or the mixer does.
  const version = $derived(project && stored ? `${stored.id}:${stored.revision}:${JSON.stringify(mixOf(project).map((t) => [t.mute, t.solo, t.volume]))}` : '');

  const loader = $derived.by(() => {
    const p = project;
    const s = stored;
    if (!p || !s) return null;
    const mix = dirty ? mixOf(p) : undefined;
    return async (start: number, length: number, signal: AbortSignal): Promise<Blob> => {
      const r = await api.preview({ project_id: s.id, start, duration: length, tracks: mix }, signal);
      lastPreview = describe(r.info);
      return r.blob;
    };
  });

  function describe(i: PreviewInfo): string {
    return `last window ${(i.ms / 1000).toFixed(2)} s${i.cache === 'hit' ? ' (cached)' : ''}`;
  }

  async function saveProject() {
    if (!project || !stored) return;
    note = '';
    noteIsError = false;
    try {
      const r = await api.projects.update(stored.id, { project: $state.snapshot(project) as Project, revision: stored.revision });
      stored = r;
      project = r.project;
      dirty = false;
      note = 'saved';
    } catch (e) {
      note = errorText(e);
      noteIsError = true;
    }
  }

  // Full-quality render as a job.
  let renderJob = $state<string | null>(null);
  let renderState = $state('');
  async function renderVideo() {
    if (!stored) return;
    note = '';
    noteIsError = false;
    try {
      if (dirty) await saveProject();
      const r = await api.projects.render(stored.id);
      renderJob = r.job_id;
      renderState = 'queued';
      subscribeTopic(`job:${r.job_id}`);
    } catch (e) {
      note = errorText(e);
      noteIsError = true;
    }
  }
  $effect(() => {
    const id = renderJob;
    const ev = id ? $jobEvents[id] : undefined;
    if (!id || !ev) return;
    renderState = ev.state === 'running' ? `${ev.message ?? 'rendering'} ${Math.round(ev.progress * 100)}%` : ev.state;
  });

  const summary = $derived.by(() => {
    if (!stored) return '';
    const bits = [stored.kind === 'song' ? 'song' : 'sequence', `${stored.clips} clips`, `${stored.duration.toFixed(1)} s`];
    if (stored.access !== 'owner') bits.push(`${stored.access} access, by ${stored.owner_name}`);
    return bits.join(' · ');
  });
</script>

<svelte:head><title>{project ? `${project.name} · studio` : 'studio'} · YTP Machine</title></svelte:head>

<TopBar>
  <VoiceSelect title="The default voice for new tracks" />
</TopBar>

<div class="subbar">
  <span class="title">studio</span>
  {#if project}
    <b class="name">{project.name}</b>
    <span class="meta">{summary}</span>
  {:else}
    <span class="meta">no project open</span>
  {/if}
  <span class="grow"></span>
  {#if lastPreview}<span class="meta">{lastPreview}</span>{/if}
  {#if note}<span class="note" class:err={noteIsError}>{note}</span>{/if}
  {#if renderJob}
    <span class="meta">render: {renderState}</span>
    {#if renderState === 'done'}<a class="btn" href={api.jobs.resultUrl(renderJob)} download>download</a>{/if}
  {/if}
  {#if project && canWrite}<button type="button" disabled={!dirty} onclick={saveProject}>save</button>{/if}
  {#if project && $can('export.render')}<button type="button" class="primary" onclick={renderVideo}>render video</button>{/if}
</div>

<div class="studio">
  {#if loadError}
    <p class="warn" role="alert">{loadError}</p>
  {/if}
  <div class="upper">
    <PreviewPane bind:playhead bind:playing {duration} load={loader} {version} note={loading ? 'loading the project…' : ''} />
    <PromptBox bind:mode bind:prompt />
  </div>

  {#if !project && !loading && recent.length}
    <section class="card recent">
      <div class="section-label">your projects</div>
      <ul>
        {#each recent as p (p.id)}
          <li>
            <button type="button" class="link" onclick={() => goto(`/?project=${p.id}`)}>{p.name}</button>
            <span class="meta">{p.kind} · {p.clips} clips · {p.duration.toFixed(1)} s{p.access !== 'owner' ? ` · ${p.access} by ${p.owner_name}` : ''}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <div class="tl">
    <div class="tl-tools">
      <span class="section-label">timeline</span>
      {#if !project}
        <span class="add">
          add track
          <button type="button" onclick={() => addTrack('video')}>+ video</button>
          <button type="button" onclick={() => addTrack('voice')}>+ voice</button>
          <button type="button" onclick={() => addTrack('audio')}>+ music</button>
          <button type="button" onclick={() => addTrack('midi')}>+ midi</button>
        </span>
        <span class="grow"></span>
        <button type="button" class="danger" disabled={!selected} onclick={removeSelected}>remove track</button>
      {/if}
    </div>
    <Timeline {tracks} {duration} {selected} bind:playhead onselect={(id) => (selected = id)} onflag={flag} empty="Add a track to begin." />
  </div>
</div>

<style>
  .subbar .name { font-size: .8rem; font-weight: 500; }
  .meta { color: var(--dim); font-size: .7rem; }
  .note { color: var(--amber); font-size: .74rem; }
  .note.err { color: var(--red); }
  .warn { color: var(--red); font-size: .78rem; margin: 0; }
  .studio { width: min(1440px, 100% - 2rem); margin: 1rem auto 3rem; display: flex; flex-direction: column; gap: 1rem; }
  .upper { display: grid; grid-template-columns: minmax(0, 1.5fr) minmax(280px, 1fr); gap: 1rem; align-items: start; }
  @media (max-width: 900px) { .upper { grid-template-columns: minmax(0, 1fr); } }
  .recent { padding: .75rem 1rem; }
  .recent ul { list-style: none; margin: .4rem 0 0; padding: 0; display: flex; flex-direction: column; gap: .3rem; }
  .recent li { display: flex; gap: .8rem; align-items: baseline; }
  .link { background: none; border: 0; color: var(--blue); padding: 0; cursor: pointer; font-size: .8rem; }
  .tl-tools { display: flex; align-items: center; gap: .6rem 1rem; flex-wrap: wrap; margin-bottom: .6rem; }
  .tl-tools .section-label { margin: 0; }
  .add { display: inline-flex; align-items: center; gap: .35rem; color: var(--dim); font-size: .72rem; }
  .add button { font-size: .72rem; padding: .22rem .55rem; }
  .grow { flex: 1; }
</style>
