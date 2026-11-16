<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { api, ApiError, errorText, type PreviewInfo } from '#lib/api.ts';
  import { can } from '#lib/auth.ts';
  import { ProjectHistory } from '#lib/history.ts';
  import { mixOf, projectDuration, toTimelineTracks, type Effect, type Place, type Project, type ProjectClip, type ProjectMeta, type StoredProject } from '#lib/project.ts';
  import { type Track, type TrackKind } from '#lib/timeline.ts';
  import { selectedVoice } from '#lib/voice.ts';
  import { jobEvents, subscribeTopic } from '#lib/ws.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import VoiceSelect from '#lib/components/VoiceSelect.svelte';
  import EffectsPanel from '#lib/components/studio/EffectsPanel.svelte';
  import PreviewPane from '#lib/components/studio/PreviewPane.svelte';
  import StartPrompt from '#lib/components/studio/StartPrompt.svelte';
  import Timeline from '#lib/components/timeline/Timeline.svelte';
  import ShareDialog from '#lib/components/ShareDialog.svelte';
  import ExportDialog from '#lib/components/ExportDialog.svelte';

  // The Studio: a multi-track arrangement over a stored project. `?project=<id>`
  // opens one (what "Open in Studio" on the Say and Song pages makes). With no
  // project open there is no timeline to pretend to edit: the page creates a
  // blank project or lists the recent ones. A project's tracks and clips are
  // drawn by time, and the preview plays the window under the playhead,
  // rendered on the server.
  const projectId = $derived.by(() => {
    const n = Number(page.url.searchParams.get('project'));
    return Number.isInteger(n) && n > 0 ? n : null;
  });

  let stored = $state<StoredProject | null>(null);
  let project = $state<Project | null>(null);
  // Undo/redo over the editable state, mirroring the Rust `History`. The
  // stack itself is not reactive; `undoable`/`redoable` expose its edges.
  let history: ProjectHistory | null = null;
  let undoable = $state(false);
  let redoable = $state(false);
  let dirty = $state(false);
  let loading = $state(false);
  let loadError = $state('');
  let recent = $state<ProjectMeta[]>([]);

  let playhead = $state(0);
  let playing = $state(false);
  let selected = $state<string | null>(null);
  // The clip whose effects the panel edits: its track and clip ids.
  let selectedClip = $state<{ track: string; clip: string } | null>(null);
  let sharing = $state(false);
  let note = $state('');
  let noteIsError = $state(false);
  let lastPreview = $state('');
  // The timing fields are drafts until their set button commits them, so a
  // half-typed number never lands in the project (or its undo history).
  let startDraft = $state<string | number>('');
  let durationDraft = $state<string | number>('');
  // The selected track's name and volume are drafts the track toolbar commits,
  // exactly like the clip's timing fields.
  let trackNameDraft = $state('');
  let trackVolumeDraft = $state<string | number>('');

  // Renaming happens in place: the subbar turns the name into a field the
  // visitor types, and only the new name is posted (crates/ytp-server/src/
  // projects.rs takes name and revision together). Deleting is the owner's
  // alone, so both wait on one busy flag while the request runs.
  let renaming = $state(false);
  let renameValue = $state('');
  let projectBusy = $state(false);

  // Load the project named in the address; list the recent ones when none is.
  $effect(() => {
    const id = projectId;
    stored = null;
    project = null;
    history = null;
    undoable = false;
    redoable = false;
    dirty = false;
    loadError = '';
    playhead = 0;
    playing = false;
    selectedClip = null;
    sharing = false;
    note = '';
    addNote = '';
    lastPreview = '';
    renaming = false;
    renameValue = '';
    projectBusy = false;
    cancelMove();
    loading = false;
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
        history = new ProjectHistory(r.project);
        syncHistory();
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

  // crates/ytp-core/src/timeline.rs: validate() rejects a document with more.
  const MAX_TRACKS = 256;

  // A blank project: one video track and one voice track, with the top bar's
  // voice as the project corpus. The server assigns the id and revision.
  const SIZES: Record<'480' | '720' | '1080', [number, number]> = {
    '480': [640, 480],
    '720': [1280, 720],
    '1080': [1920, 1080]
  };
  let newName = $state('');
  let newSize = $state<'480' | '720' | '1080'>('480');
  let creating = $state(false);
  let createError = $state('');

  async function createProject(e: SubmitEvent) {
    e.preventDefault();
    if (creating) return;
    const name = newName.trim() || 'untitled';
    const [width, height] = SIZES[newSize];
    const p: Project = {
      version: 1, // crates/ytp-core/src/timeline.rs: PROJECT_VERSION
      name,
      corpus: $selectedVoice || '',
      fps: 30,
      width,
      height,
      captions: false,
      tracks: [
        { id: 'v1', kind: 'video', name: 'Video 1', mute: false, solo: false, volume: 1, generated: false, clips: [] },
        { id: 'a1', kind: 'audio', name: 'Voice 1', mute: false, solo: false, volume: 1, generated: false, clips: [] }
      ]
    };
    creating = true;
    createError = '';
    try {
      const r = await api.projects.create(p, name);
      newName = '';
      await goto(`/?project=${r.id}`);
    } catch (err) {
      createError = errorText(err);
    } finally {
      creating = false;
    }
  }

  // With a project open the track is a real ProjectTrack (video or audio;
  // voice/music both map to the project's audio kind), so it can be saved.
  function addTrack(kind: TrackKind) {
    if (!project) return;
    const pk = kind === 'video' ? 'video' : kind === 'voice' || kind === 'audio' ? 'audio' : null;
    if (!pk || !canWrite || project.tracks.length >= MAX_TRACKS) return;
    const label = pk === 'video' ? 'Video' : kind === 'voice' ? 'Voice' : 'Music';
    const n = project.tracks.filter((t) => t.kind === pk).length + 1;
    const base = pk === 'video' ? 'v' : 'a';
    let id = `${base}${n}`;
    for (let i = n + 1; project.tracks.some((t) => t.id === id); i += 1) id = `${base}${i}`;
    project.tracks.push({ id, kind: pk, name: `${label} ${n}`, mute: false, solo: false, volume: 1, generated: false, clips: [] });
    selected = id;
    dirty = true;
    record();
  }

  // The selected track, when it can host a generated clip: the server expands
  // one on a video track only (crates/ytp-server/src/projects.rs).
  const videoTrack = $derived(project?.tracks.find((t) => t.id === selected && t.kind === 'video') ?? null);

  // The selected track whatever its kind: the track toolbar renames, volumes,
  // reorders and deletes it.
  const selectedTrack = $derived(project?.tracks.find((t) => t.id === selected) ?? null);

  // A generated clip is the cheapest clip to author: a prompt and a seed, and
  // the server cuts it at render/preview time. Its duration here is only a
  // placeholder - the expansion's real length wins and later clips on the track
  // shift to fit - so the panel says that instead of pretending to know the
  // length. Only `ytp` is offered: the server expands no other kind yet.
  const GENERATED_PLACEHOLDER = 2;
  // The shortest clip the Studio writes (crates/ytp-core/src/timeline.rs wants
  // duration > 0; anything below this is no clip at all).
  const MIN_CLIP = 0.05;
  // Why the last "+ generated" click added nothing; shown by the toolbar.
  let addNote = $state('');
  // What the next "+ generated" clip should say; cleared once it is added.
  let generatedPrompt = $state('');

  // The room a clip added at `at` has on a track: up to the start of the next
  // clip that begins at or after it (Infinity when there is none), or null when
  // `at` is inside a clip, where there is no room at all. The renderer refuses
  // same-track overlaps (crates/ytp-render/src/sequence.rs) but validate() does
  // not, so the placeholder has to fit this gap: the longer real length only
  // shifts clips that start later.
  function roomAt(clips: ProjectClip[], at: number): number | null {
    let room = Infinity;
    for (const c of clips) {
      if (at > c.start && at < c.start + c.duration) return null;
      if (c.start >= at) room = Math.min(room, c.start - at);
    }
    return room;
  }

  function addGeneratedClip() {
    const track = videoTrack;
    if (!project || !canWrite || !track) return;
    const start = Number.isFinite(playhead) ? Math.max(0, playhead) : 0;
    const room = roomAt(track.clips, start);
    const duration = room === null ? 0 : Math.min(GENERATED_PLACEHOLDER, room);
    if (duration < MIN_CLIP) {
      addNote = 'no room for a new clip here';
      return;
    }
    const used = new Set(project.tracks.flatMap((t) => t.clips.map((c) => c.id)));
    // The id style plans use (crates/ytp-splice/src/project.rs: `c{i}`), first free.
    let n = 0;
    while (used.has(`c${n}`)) n += 1;
    const id = `c${n}`;
    addNote = '';
    track.clips.push({
      id,
      start,
      duration,
      source: { type: 'generated', kind: 'ytp', prompt: generatedPrompt.trim(), options: {}, seed: '', corpus: undefined },
      effects: []
    });
    generatedPrompt = '';
    selected = track.id;
    selectedClip = { track: track.id, clip: id };
    dirty = true;
    record();
  }

  const tracks = $derived<Track[]>(project ? toTimelineTracks(project) : []);
  const duration = $derived(project ? Math.max(1, projectDuration(project) + (stored?.kind === 'song' ? 0.8 : 0)) : 0);
  const canWrite = $derived(!!stored && stored.access !== 'read' && $can('project.write'));
  const tooManyTracks = $derived(!!project && project.tracks.length >= MAX_TRACKS);

  // The clip under the timeline selection, or null when none is selected (or
  // the one that was is gone).
  const selection = $derived.by(() => {
    const s = selectedClip;
    if (!project || !s) return null;
    const track = project.tracks.find((t) => t.id === s.track);
    return track?.clips.find((c) => c.id === s.clip) ?? null;
  });

  // Keep the timing drafts in step with the selected clip: a drag, an undo or
  // opening another project changes start/duration too. Typing does not touch
  // the model, so this never fights the visitor while the field is being edited.
  $effect(() => {
    const s = selection;
    if (!s) return;
    startDraft = s.start.toFixed(3);
    durationDraft = s.duration.toFixed(3);
  });

  // Same for the selected track: a new selection, an undo or another project
  // puts its real name and volume back in the fields before the visitor types.
  $effect(() => {
    const t = selectedTrack;
    if (!t) return;
    trackNameDraft = t.name;
    trackVolumeDraft = t.volume;
  });

  // Effects edits from the panel: change the clip, then record the state so
  // undo brings the old effects back; `dirty` makes the save button persist it.
  function setEffects(effects: Effect[]) {
    if (!project || !selectedClip) return;
    const { track: trackId, clip: clipId } = selectedClip;
    const track = project.tracks.find((t) => t.id === trackId);
    const clip = track?.clips.find((c) => c.id === clipId);
    if (!clip) return;
    clip.effects = effects;
    dirty = true;
    record();
  }

  // The picture-in-picture box of the selected clip: the panel offers fixed
  // boxes only, so the server's place checks cannot be broken. Clearing the box
  // is `undefined` (never written), which hands the clip back to the renderer's
  // default. One snapshot per pick, exactly like an effects edit; the snapshot
  // holds the whole project, so undo restores the previous box (or its absence).
  function setPlace(place: Place | undefined) {
    if (!project || !selectedClip) return;
    const { track: trackId, clip: clipId } = selectedClip;
    const track = project.tracks.find((t) => t.id === trackId);
    const clip = track?.clips.find((c) => c.id === clipId);
    if (!clip) return;
    clip.place = place;
    dirty = true;
    record();
  }

  // The whole source of a generated clip is its prompt and seed: the panel has
  // already clamped both to what the server accepts (4000 characters, a seed of
  // at most 64 printable bytes), so this only stores them. The source object is
  // replaced, as `setEffects` replaces the effects array, and one snapshot is
  // taken per committed edit.
  function setGenerated(field: 'prompt' | 'seed', value: string) {
    if (!project || !selectedClip) return;
    const { track: trackId, clip: clipId } = selectedClip;
    const track = project.tracks.find((t) => t.id === trackId);
    const clip = track?.clips.find((c) => c.id === clipId);
    if (!clip || clip.source.type !== 'generated') return;
    const source = clip.source;
    if (source[field] === value) return;
    clip.source = field === 'prompt' ? { ...source, prompt: value } : { ...source, seed: value };
    dirty = true;
    record();
  }

  // Moving the selected clip to another track of its own kind (video to video,
  // audio to audio). The clip keeps its time, so it may now overlap a clip on
  // its new track: that is legal and composites as picture-in-picture. Only the
  // clip object moves, ids and all, so nothing is clamped or renumbered.
  function neighborTrack(dir: -1 | 1): { id: string; name: string } | null {
    const s = selectedClip;
    if (!project || !s) return null;
    const at = project.tracks.findIndex((t) => t.id === s.track);
    if (at < 0) return null;
    const kind = project.tracks[at].kind;
    for (let i = at + dir; i >= 0 && i < project.tracks.length; i += dir) {
      const t = project.tracks[i];
      if (t.kind === kind) return { id: t.id, name: t.name || t.id };
    }
    return null;
  }
  const upTrack = $derived.by(() => neighborTrack(-1));
  const downTrack = $derived.by(() => neighborTrack(1));

  function moveSelectedTo(trackId: string) {
    const s = selectedClip;
    if (!project || !s || !canWrite) return;
    const from = project.tracks.find((t) => t.id === s.track);
    const to = project.tracks.find((t) => t.id === trackId);
    if (!from || !to || from.id === to.id || from.kind !== to.kind) return;
    const i = from.clips.findIndex((c) => c.id === s.clip);
    if (i < 0) return;
    const [clip] = from.clips.splice(i, 1);
    to.clips.push(clip);
    selectedClip = { track: to.id, clip: clip.id };
    selected = to.id;
    dirty = true;
    record();
  }

  const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

  // The room the selected clip has on its own track between its neighbours,
  // mirroring the Timeline's `room()`: `lo` is the end of the last clip that
  // ends before `start`, `hi` the start of the first that begins at or after
  // `end` (Infinity when there is none), so a numeric edit can never make two
  // clips on one track overlap - the renderer refuses those.
  function gapAround(track: { clips: ProjectClip[] }, clipId: string, start: number, end: number): { lo: number; hi: number } {
    let lo = 0;
    let hi = Infinity;
    for (const o of track.clips) {
      if (o.id === clipId) continue;
      const oEnd = o.start + o.duration;
      if (oEnd <= start + 1e-9) lo = Math.max(lo, oEnd);
      else if (o.start >= end - 1e-9) hi = Math.min(hi, o.start);
    }
    lo = Math.max(0, lo);
    return { lo, hi: Math.max(lo, hi) };
  }

  // Commit a typed start or length: clamped to the gap on its track, then one
  // undo snapshot per committed change. The draft is written back from the
  // model so a clamped (or rejected) value never stays on screen as a lie.
  function applyTiming(field: 'start' | 'duration', raw: string) {
    if (!project || !selection || !selectedClip || !canWrite) return;
    const s = selectedClip;
    const track = project.tracks.find((t) => t.id === s.track);
    if (!track) return;
    const clip = track.clips.find((c) => c.id === s.clip);
    if (!clip) return;
    const n = Number(raw);
    const current = field === 'start' ? clip.start : clip.duration;
    if (!Number.isFinite(n)) {
      if (field === 'start') startDraft = current.toFixed(3);
      else durationDraft = current.toFixed(3);
      return;
    }
    let changed = false;
    if (field === 'start') {
      const g = gapAround(track, clip.id, clip.start, clip.start + clip.duration);
      const hi = g.hi === Infinity ? Math.max(g.lo, n) : Math.max(g.lo, g.hi - clip.duration);
      const start = clamp(n, g.lo, hi);
      if (start !== clip.start) {
        clip.start = start;
        changed = true;
      }
    } else {
      const g = gapAround(track, clip.id, clip.start, clip.start + clip.duration);
      const max = g.hi === Infinity ? Math.max(MIN_CLIP, n) : Math.max(MIN_CLIP, g.hi - clip.start);
      const duration = clamp(n, MIN_CLIP, max);
      if (duration !== clip.duration) {
        clip.duration = duration;
        changed = true;
      }
    }
    if (changed) {
      dirty = true;
      record();
    }
    if (field === 'start') startDraft = clip.start.toFixed(3);
    else durationDraft = clip.duration.toFixed(3);
  }

  // Remove the selected clip: the row loses it, and the selection forgets it.
  function deleteClip() {
    if (!project || !selection || !selectedClip || !canWrite) return;
    const s = selectedClip;
    const track = project.tracks.find((t) => t.id === s.track);
    if (!track) return;
    const i = track.clips.findIndex((c) => c.id === s.clip);
    if (i < 0) return;
    track.clips.splice(i, 1);
    selectedClip = null;
    dirty = true;
    record();
  }

  // Copy the selected clip to the end of its own track. The new clip starts
  // after the last one, so it can never overlap (the renderer refuses same-track
  // overlaps), and it takes the first free `c<n>` id so it is distinct. The copy
  // keeps everything the original carried - duration, source, effects, place,
  // caption and label - and the duplicate becomes the selection.
  function duplicateClip() {
    if (!project || !selection || !selectedClip || !canWrite) return;
    const s = selectedClip;
    const track = project.tracks.find((t) => t.id === s.track);
    if (!track) return;
    const clip = track.clips.find((c) => c.id === s.clip);
    if (!clip) return;
    const used = new Set(project.tracks.flatMap((t) => t.clips.map((c) => c.id)));
    let n = 0;
    while (used.has(`c${n}`)) n += 1;
    const id = `c${n}`;
    const start = track.clips.reduce((m, c) => Math.max(m, c.start + c.duration), 0);
    const copy = structuredClone($state.snapshot(clip)) as ProjectClip;
    copy.id = id;
    copy.start = start;
    track.clips.push(copy);
    selected = track.id;
    selectedClip = { track: track.id, clip: id };
    dirty = true;
    record();
  }

  // A clip dragged or trimmed on the timeline. The move lands in the project
  // and marks it unsaved; a drag fires many events, so the whole drag leaves at
  // most one undo snapshot. The timer is a fallback for a drag that pauses or
  // ends without its release event; once it has taken the drag's snapshot it
  // only refreshes it, so a 12-step drag still needs one Ctrl+Z.
  let moveTimer: ReturnType<typeof setTimeout> | undefined;
  let movePending = false; // this drag has changed the project
  let moveRecorded = false; // ... and the drag's one snapshot exists

  function moveClip(trackId: string, clipId: string, start: number, length: number) {
    if (!project) return;
    const track = project.tracks.find((t) => t.id === trackId);
    const clip = track?.clips.find((c) => c.id === clipId);
    if (!clip || !Number.isFinite(start) || !Number.isFinite(length)) return;
    const s = Math.max(0, start);
    const d = Math.max(0.05, length);
    if (s === clip.start && d === clip.duration) return;
    clip.start = s;
    clip.duration = d;
    dirty = true;
    movePending = true;
    if (moveTimer !== undefined) clearTimeout(moveTimer);
    moveTimer = setTimeout(() => {
      moveTimer = undefined;
      if (!movePending) return;
      movePending = false;
      if (moveRecorded) replaceMove();
      else {
        moveRecorded = true;
        record();
      }
    }, 250);
  }

  // Refresh the drag's snapshot in place, so intermediate states of the same
  // gesture never become undo steps of their own.
  function replaceMove() {
    if (!project || !history) return;
    history.replace($state.snapshot(project) as Project);
    syncHistory();
  }

  // The drag ended: drop the pending timer and leave one snapshot holding the
  // final state - either the first one recorded now, or the mid-drag one
  // refreshed. Never pushes a second snapshot for the same drag.
  function endClipMove() {
    const recorded = moveRecorded;
    const pending = movePending;
    cancelMove();
    if (recorded) replaceMove();
    else if (pending) record();
  }

  // An undo/redo/new project cancels a snapshot still waiting on that timer.
  function cancelMove() {
    if (moveTimer !== undefined) clearTimeout(moveTimer);
    moveTimer = undefined;
    movePending = false;
    moveRecorded = false;
  }

  function flag(id: string, f: 'mute' | 'solo') {
    if (!project) return;
    const t = project.tracks.find((x) => x.id === id);
    if (t) {
      t[f] = !t[f];
      dirty = true;
      record();
    }
  }

  // The selected track's own edits, one undo snapshot each: a name, a volume,
  // the track order (the order of `tracks` is the layer order the renderer
  // composites in) or the track and every clip it carried.

  // Rename in the document only: the track's name is local to the project, so
  // nothing is posted and no request flag is needed.
  function renameTrack() {
    if (!project || !selectedTrack || !canWrite) return;
    const name = trackNameDraft.trim();
    if (!name || name.length > 120) return;
    if (name === selectedTrack.name) return;
    selectedTrack.name = name;
    trackNameDraft = name;
    dirty = true;
    record();
  }

  // Volume is 0..4, 1 being unity. A value that is not a number leaves the
  // track alone and puts the real volume back in the field, and a clamped one
  // is written back too, so the draft never shows a lie.
  function setTrackVolume() {
    if (!project || !selectedTrack || !canWrite) return;
    const n = Number(trackVolumeDraft);
    if (!Number.isFinite(n)) {
      trackVolumeDraft = selectedTrack.volume;
      return;
    }
    const v = Math.min(4, Math.max(0, n));
    if (v === selectedTrack.volume) {
      trackVolumeDraft = v;
      return;
    }
    selectedTrack.volume = v;
    trackVolumeDraft = v;
    dirty = true;
    record();
  }

  // Swap the track with its neighbour above or below. The clips keep their
  // places, so only what covers what changes.
  function moveTrack(dir: -1 | 1) {
    if (!project || !selectedTrack || !canWrite) return;
    const i = project.tracks.findIndex((t) => t.id === selectedTrack.id);
    const to = i + dir;
    if (i < 0 || to < 0 || to >= project.tracks.length) return;
    const other = project.tracks[to];
    project.tracks[to] = project.tracks[i];
    project.tracks[i] = other;
    dirty = true;
    record();
  }

  // Deleting a track drops its clips too, which no undo can bring back once
  // the save has posted it, so the visitor has to confirm the loss.
  function deleteTrack() {
    if (!project || !selectedTrack || !canWrite) return;
    const n = selectedTrack.clips.length;
    const name = selectedTrack.name || selectedTrack.id;
    const sure = n > 0 ? confirm(`Delete ${name} and its ${n} clip(s)?`) : confirm(`Delete ${name}?`);
    if (!sure) return;
    const i = project.tracks.findIndex((t) => t.id === selectedTrack.id);
    if (i < 0) return;
    project.tracks.splice(i, 1);
    if (selectedClip && selectedClip.track === selectedTrack.id) selectedClip = null;
    selected = null;
    dirty = true;
    record();
  }

  // Mirror the history edges into reactive flags so the toolbar can react.
  function syncHistory() {
    undoable = history?.canUndo() ?? false;
    redoable = history?.canRedo() ?? false;
  }

  // Record the current project as the newest undoable state.
  function record() {
    if (!project) return;
    history?.apply($state.snapshot(project) as Project);
    syncHistory();
  }

  function undo() {
    if (!project || !history) return;
    cancelMove();
    const p = history.undo();
    if (!p) return;
    project = p;
    dirty = true;
    syncHistory();
  }

  function redo() {
    if (!project || !history) return;
    cancelMove();
    const p = history.redo();
    if (!p) return;
    project = p;
    dirty = true;
    syncHistory();
  }

  // Undo/redo keys while a project is open; the listener goes away with the
  // page (or when another project is opened). Typing fields keep their own
  // Ctrl+Z.
  $effect(() => {
    if (!project) return;
    function onKey(e: KeyboardEvent) {
      if (!project || !history || e.isComposing) return;
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || t?.isContentEditable) return;
      if (!(e.ctrlKey || e.metaKey)) return;
      const key = e.key.toLowerCase();
      if (key === 'arrowup' || key === 'arrowdown') {
        const to = key === 'arrowup' ? upTrack : downTrack;
        if (!to) return;
        e.preventDefault();
        moveSelectedTo(to.id);
      } else if (key === 'z' && !e.shiftKey) {
        e.preventDefault();
        undo();
      } else if ((key === 'z' && e.shiftKey) || key === 'y') {
        e.preventDefault();
        redo();
      }
    }
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

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

  // Rename in place: the subbar swaps the name for a field the visitor types, so
  // the new name can be checked before it is posted, and cancel leaves the
  // project untouched.
  function startRename() {
    renaming = true;
    renameValue = project?.name ?? '';
  }

  function cancelRename() {
    renaming = false;
  }

  async function saveRename(e: SubmitEvent) {
    e.preventDefault();
    const name = renameValue.trim();
    if (!stored || !name || projectBusy) return;
    projectBusy = true;
    note = '';
    noteIsError = false;
    try {
      const r = await api.projects.update(stored.id, { name, revision: stored.revision });
      stored = r;
      project = r.project;
      renaming = false;
      note = 'renamed';
    } catch (err) {
      note = errorText(err);
      noteIsError = true;
    } finally {
      projectBusy = false;
    }
  }

  // Delete the whole project, which no undo can bring back: it asks for
  // confirmation and then leaves the page, whose project no longer exists.
  async function deleteProject() {
    if (!stored || !project) return;
    if (!confirm(`Delete "${project.name}"? This cannot be undone.`)) return;
    projectBusy = true;
    note = '';
    noteIsError = false;
    try {
      await api.projects.remove(stored.id);
      await goto('/');
    } catch (err) {
      note = errorText(err);
      noteIsError = true;
    } finally {
      projectBusy = false;
    }
  }

  // Full-quality render as a job.
  let renderJob = $state<string | null>(null);
  let renderState = $state('');
  // A render this job is still running: it can be cancelled and no other may
  // start until it ends.
  let renderActive = $state(false);
  // The export dialog over the finished job, opened by the subbar's export.
  let exporting = $state(false);
  async function renderVideo() {
    if (!stored) return;
    note = '';
    noteIsError = false;
    exporting = false;
    try {
      if (dirty) await saveProject();
      const r = await api.projects.render(stored.id);
      renderJob = r.job_id;
      renderState = 'queued';
      renderActive = true;
      subscribeTopic(`job:${r.job_id}`);
    } catch (e) {
      renderActive = false;
      note = errorText(e);
      noteIsError = true;
    }
  }

  // Stop the render in flight. Cancelling a finished job deletes its video, so
  // this is only offered while one runs.
  async function cancelRender() {
    const id = renderJob;
    if (!id) return;
    try {
      await api.jobs.cancel(id);
      renderState = 'cancelled';
      renderActive = false;
    } catch (e) {
      note = errorText(e);
      noteIsError = true;
    }
  }

  $effect(() => {
    const id = renderJob;
    const ev = id ? $jobEvents[id] : undefined;
    if (!id || !ev) return;
    renderActive = ev.state !== 'done' && ev.state !== 'failed' && ev.state !== 'cancelled';
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
    {#if renaming && canWrite}
      <form class="rename" onsubmit={saveRename}>
        <input type="text" bind:value={renameValue} maxlength="120" placeholder="new name"
               aria-label="New project name" disabled={projectBusy} />
        <button type="submit" disabled={projectBusy || !renameValue.trim()}
                title="Save the new project name">{projectBusy ? 'renaming…' : 'save name'}</button>
        <button type="button" title="Keep the current name" onclick={cancelRename}>cancel</button>
      </form>
    {:else}
      <b class="name">{project.name}</b>
    {/if}
    <span class="meta">{summary}</span>
    {#if canWrite && !renaming}<button type="button" title="Rename this project" onclick={startRename}>rename</button>{/if}
    {#if stored?.access === 'owner' && canWrite}<button type="button" class="danger" disabled={projectBusy} title="Delete this project" onclick={deleteProject}>delete</button>{/if}
  {:else}
    <span class="meta">no project open</span>
  {/if}
  <span class="grow"></span>
  {#if lastPreview}<span class="meta">{lastPreview}</span>{/if}
  {#if note}<span class="note" class:err={noteIsError}>{note}</span>{/if}
  {#if renderJob}
    <span class="meta">render: {renderState}</span>
    {#if renderActive}<button type="button" title="Cancel the render in flight" onclick={cancelRender}>cancel</button>{/if}
    {#if renderState === 'done'}
      <a class="btn" href={api.jobs.resultUrl(renderJob)} download>download</a>
      <button type="button" title="Re-encode this video with another preset" onclick={() => (exporting = true)}>export</button>
    {/if}
  {/if}
  {#if project && canWrite}<button type="button" disabled={!undoable} title="Undo (Ctrl+Z)" onclick={undo}>undo</button>{/if}
  {#if project && canWrite}<button type="button" disabled={!redoable} title="Redo (Ctrl+Shift+Z)" onclick={redo}>redo</button>{/if}
  {#if project && canWrite}<button type="button" disabled={!dirty} onclick={saveProject}>save</button>{/if}
  {#if stored && stored.access === 'owner' && $can('project.share')}<button type="button" onclick={() => (sharing = true)}>share</button>{/if}
  {#if project && $can('export.render')}<button type="button" class="primary" disabled={renderActive} onclick={renderVideo}>render video</button>{/if}
</div>

<div class="studio">
  {#if loadError}
    <p class="warn" role="alert">{loadError}</p>
  {/if}

  {#if project}
    <PreviewPane bind:playhead bind:playing {duration} load={loader} {version} />

    <div class="tl">
      <div class="tl-tools">
        <span class="section-label">timeline</span>
        {#if canWrite}
          <span class="add">
            add track
            <button type="button" disabled={tooManyTracks} onclick={() => addTrack('video')}>+ video</button>
            <button type="button" disabled={tooManyTracks} onclick={() => addTrack('voice')}>+ voice</button>
            <button type="button" disabled={tooManyTracks} onclick={() => addTrack('audio')}>+ music</button>
          </span>
        {:else}
          <span class="meta">read-only</span>
        {/if}
        <span class="add">
          add clip
          <input type="text" class="gen-prompt" bind:value={generatedPrompt} maxlength="4000"
                 placeholder="what should it say?" aria-label="Generated clip prompt"
                 disabled={!canWrite || !videoTrack} />
          <button type="button" disabled={!canWrite || !videoTrack}
                  title={!canWrite ? 'Read-only access' : videoTrack ? 'A generated clip: the placeholder is clamped to the gap before the next clip; the real length is decided when it renders' : 'Select a video track first'}
                  onclick={addGeneratedClip}>+ generated</button>
          {#if addNote}<span class="note" role="status">{addNote}</span>{/if}
        </span>
      </div>
      <Timeline {tracks} {duration} {selected} selectedClip={selectedClip?.clip ?? null} bind:playhead
                onselect={(id) => { selected = id; if (selectedClip && selectedClip.track !== id) selectedClip = null; }}
                onselectclip={(track, clip) => { selected = track; selectedClip = { track, clip }; }}
                onclipmove={canWrite ? moveClip : undefined}
                onclipmoveend={canWrite ? endClipMove : undefined}
                onflag={flag} empty="Add a track to begin." />
    </div>

    {#if project && selectedTrack}
      <div class="tl-tools">
        <span class="section-label">selected track</span>
        {#if canWrite}
          <label class="timing">name
            <input type="text" maxlength="120" bind:value={trackNameDraft} aria-label="Track name" />
            <button type="button" title="Rename this track" onclick={renameTrack}>set</button>
          </label>
          <label class="timing">volume
            <input type="number" min="0" max="4" step="0.05" bind:value={trackVolumeDraft} aria-label="Track volume" />
            <button type="button" title="Set this track's volume" onclick={setTrackVolume}>set</button>
          </label>
          <span class="add">
            move
            <button type="button" title="Move track up" onclick={() => moveTrack(-1)}>↑</button>
            <button type="button" title="Move track down" onclick={() => moveTrack(1)}>↓</button>
          </span>
          <button type="button" class="danger" title="Delete this track and its clips"
                  onclick={deleteTrack}>delete track</button>
        {:else}
          <span class="meta">read-only</span>
        {/if}
      </div>
    {/if}

    {#if selection}
      <div class="tl-tools">
        <span class="section-label">selected clip</span>
        <span class="add">
          move to track
          <button type="button" disabled={!canWrite || !upTrack} title="Move to the track above (Ctrl+↑)"
                  onclick={() => { const t = upTrack; if (t) moveSelectedTo(t.id); }}>↑ {upTrack?.name ?? '—'}</button>
          <button type="button" disabled={!canWrite || !downTrack} title="Move to the track below (Ctrl+↓)"
                  onclick={() => { const t = downTrack; if (t) moveSelectedTo(t.id); }}>↓ {downTrack?.name ?? '—'}</button>
        </span>
        <label class="timing">start
          <input type="number" min="0" step="0.01" bind:value={startDraft} disabled={!canWrite}
                 onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); applyTiming('start', String(startDraft)); } }}
                 aria-label="Clip start, seconds" />
          <button type="button" disabled={!canWrite} title="Set the clip start" onclick={() => applyTiming('start', String(startDraft))}>set</button>
        </label>
        <label class="timing">length
          <input type="number" min="0" step="0.01" bind:value={durationDraft} disabled={!canWrite}
                 onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); applyTiming('duration', String(durationDraft)); } }}
                 aria-label="Clip length, seconds" />
          <button type="button" disabled={!canWrite} title="Set the clip length" onclick={() => applyTiming('duration', String(durationDraft))}>set</button>
        </label>
        <span class="add">
          <button type="button" class="danger" disabled={!canWrite} title="Remove this clip from its track"
                  onclick={deleteClip}>delete clip</button>
          <button type="button" disabled={!canWrite} title="Copy this clip to the end of its track"
                  onclick={duplicateClip}>duplicate</button>
        </span>
        <span class="meta">a clip may overlap one on its new track; that renders as picture-in-picture</span>
      </div>
      <EffectsPanel clip={selection} editable={canWrite} onchange={setEffects} onplace={setPlace} ongenerated={setGenerated} />
    {:else}
      <p class="meta">Select a clip in the timeline to edit its effects.</p>
    {/if}
  {:else if !loading && !loadError}
    <div class="start">
      <section class="card start-card">
        <div class="section-label">new project</div>
        <form onsubmit={createProject}>
          <label>name
            <input type="text" bind:value={newName} maxlength="120" placeholder="untitled" disabled={creating} />
          </label>
          <label>size
            <select bind:value={newSize} disabled={creating}>
              <option value="480">480p · 640 × 480</option>
              <option value="720">720p · 1280 × 720</option>
              <option value="1080">1080p · 1920 × 1080</option>
            </select>
          </label>
          <p class="hint">voice: {$selectedVoice || 'none installed'} · 30 fps · starts with one video and one voice track</p>
          {#if createError}<p class="warn" role="alert">{createError}</p>{/if}
          <div><button type="submit" class="primary" disabled={creating}>{creating ? 'creating…' : 'create project'}</button></div>
        </form>
      </section>

      <div class="start-side">
        <StartPrompt />

        {#if recent.length}
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
      </div>
    </div>
  {/if}

  {#if sharing && stored}
    <ShareDialog projectId={stored.id} onclose={() => (sharing = false)} />
  {/if}

  {#if exporting && renderJob}
    <ExportDialog job_id={renderJob} onclose={() => (exporting = false)} />
  {/if}
</div>

<style>
  .subbar .name { font-size: .8rem; font-weight: 500; }
  .rename { display: inline-flex; align-items: center; gap: .3rem; }
  .rename input { max-width: 14rem; }
  .rename button { font-size: .72rem; padding: .22rem .55rem; }
  .meta { color: var(--dim); font-size: .7rem; }
  .note { color: var(--amber); font-size: .74rem; }
  .note.err { color: var(--red); }
  .warn { color: var(--red); font-size: .78rem; margin: 0; }
  .studio { width: min(1440px, 100% - 2rem); margin: 1rem auto 3rem; display: flex; flex-direction: column; gap: 1rem; }
  .start { display: grid; grid-template-columns: minmax(0, 1.15fr) minmax(0, 1fr); gap: 1rem; align-items: start; }
  .start-side { display: flex; flex-direction: column; gap: 1rem; }
  .start-card { padding: 1rem; display: flex; flex-direction: column; gap: .75rem; }
  .start-card form { display: flex; flex-direction: column; gap: .6rem; }
  .start-card label { display: flex; flex-direction: column; gap: .25rem; color: var(--muted); font-size: .72rem; }
  .start-card input, .start-card select { max-width: 24rem; }
  .start-card .hint { margin: 0; color: var(--dim); font-size: .72rem; }
  @media (max-width: 900px) { .start { grid-template-columns: minmax(0, 1fr); } }
  .recent { padding: .75rem 1rem; }
  .recent ul { list-style: none; margin: .4rem 0 0; padding: 0; display: flex; flex-direction: column; gap: .3rem; }
  .recent li { display: flex; gap: .8rem; align-items: baseline; }
  .link { background: none; border: 0; color: var(--blue); padding: 0; cursor: pointer; font-size: .8rem; }
  .tl-tools { display: flex; align-items: center; gap: .6rem 1rem; flex-wrap: wrap; margin-bottom: .6rem; }
  .tl-tools .section-label { margin: 0; }
  .add { display: inline-flex; align-items: center; gap: .35rem; color: var(--dim); font-size: .72rem; }
  .add button { font-size: .72rem; padding: .22rem .55rem; }
  .gen-prompt { width: 14rem; font-size: .72rem; padding: .2rem .45rem; }
  .timing { display: inline-flex; align-items: center; gap: .3rem; color: var(--dim); font-size: .72rem; }
  .timing input { width: 6rem; }
  .timing button { font-size: .68rem; padding: .15rem .4rem; }
  .tl-tools input[type=text] { width: 12rem; }
  .grow { flex: 1; }
</style>
