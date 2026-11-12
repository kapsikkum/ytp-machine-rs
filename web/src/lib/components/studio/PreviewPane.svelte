<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { errorText } from '#lib/api.ts';
  import { clock } from '#lib/timeline.ts';

  // Plays a project by rendering it a window at a time on the server
  // (POST /api/preview): the window under the playhead is shown while the next
  // one is fetched into the hidden second <video>, and the two swap at the
  // seam. Scrubbing fetches the window the playhead lands in; windows are
  // aligned, so a replay (or the same spot after a scrub) is a cache hit
  // here and on the server.
  const WINDOW = 8;

  type Loader = (start: number, length: number, signal: AbortSignal) => Promise<Blob>;
  type Entry = { start: number; url?: string; err?: string; done: Promise<void>; abort: AbortController };

  let {
    playhead = $bindable(0),
    duration,
    playing = $bindable(false),
    load = null,
    version = '',
    note = ''
  }: { playhead?: number; duration: number; playing?: boolean; load?: Loader | null; version?: string; note?: string } = $props();

  const cache = new Map<number, Entry>();
  let videos = $state<HTMLVideoElement[]>([]);
  let front = $state(0);
  let current = -1;
  let buffering = $state(false);
  let error = $state('');
  let reported = 0;
  let shows = 0;
  let timer: ReturnType<typeof setInterval> | undefined;
  let seen = '\u0000';

  const count = $derived(Math.max(1, Math.ceil(duration / WINDOW)));
  const ready = $derived(!!load && duration > 0);

  function ensure(idx: number): Entry | null {
    if (!load || idx < 0 || idx >= count) return null;
    const have = cache.get(idx);
    if (have) return have;
    const abort = new AbortController();
    const entry: Entry = { start: idx * WINDOW, abort, done: Promise.resolve() };
    entry.done = load(entry.start, WINDOW, abort.signal).then(
      (blob) => {
        entry.url = URL.createObjectURL(blob);
      },
      (e) => {
        if (!abort.signal.aborted) entry.err = errorText(e);
      }
    );
    cache.set(idx, entry);
    return entry;
  }

  function drop(idx: number, e: Entry) {
    e.abort.abort();
    if (e.url) URL.revokeObjectURL(e.url);
    cache.delete(idx);
  }

  /** Keep the windows near `idx`; free the rest. */
  function trim(idx: number) {
    for (const [i, e] of [...cache]) if (i < idx - 1 || i > idx + 2) drop(i, e);
  }

  function clear() {
    shows += 1;
    for (const [i, e] of [...cache]) drop(i, e);
    for (const v of videos) {
      v?.pause();
      v?.removeAttribute('src');
      v?.load();
    }
    current = -1;
  }

  function metadata(v: HTMLVideoElement): Promise<void> {
    if (v.readyState >= 1) return Promise.resolve();
    return new Promise((resolve) => v.addEventListener('loadedmetadata', () => resolve(), { once: true }));
  }

  function preloadNext(idx: number) {
    const n = ensure(idx + 1);
    if (!n) return;
    void n.done.then(() => {
      const back = videos[1 - front];
      if (current === idx && n.url && back && back.src !== n.url) back.src = n.url;
    });
  }

  /** Put window `idx` on screen at `offset` seconds into it. */
  async function show(idx: number, offset: number) {
    const token = ++shows;
    const e = ensure(idx);
    if (!e) return;
    buffering = true;
    error = '';
    videos[front]?.pause();
    trim(idx);
    await e.done;
    if (token !== shows) return;
    buffering = false;
    if (!e.url) {
      error = e.err ?? 'the preview did not come';
      playing = false;
      return;
    }
    const v = videos[front];
    current = idx;
    if (v.src !== e.url) v.src = e.url;
    await metadata(v);
    if (token !== shows) return;
    v.currentTime = Math.min(offset, Math.max(0, (v.duration || offset) - 0.04));
    reported = idx * WINDOW + v.currentTime;
    if (playing) void v.play().catch(() => (playing = false));
    preloadNext(idx);
  }

  function seek(t: number) {
    const idx = Math.min(count - 1, Math.floor(Math.max(0, t) / WINDOW));
    const v = videos[front];
    reported = t;
    if (idx === current && v && v.readyState >= 1) {
      v.currentTime = Math.max(0, t - idx * WINDOW);
      if (playing) void v.play().catch(() => (playing = false));
    } else {
      void show(idx, t - idx * WINDOW);
    }
  }

  function advance() {
    const next = current + 1;
    const front_ = videos[front];
    if (next >= count) {
      front_.pause();
      playing = false;
      playhead = duration;
      return;
    }
    const e = cache.get(next);
    const back = videos[1 - front];
    if (e?.url && back.src === e.url && back.readyState >= 2) {
      front_.pause();
      front = 1 - front;
      current = next;
      back.currentTime = 0;
      void back.play().catch(() => (playing = false));
      preloadNext(next);
      trim(next);
    } else {
      front_.pause();
      void show(next, 0);
    }
  }

  function tick() {
    const v = videos[front];
    if (!v || current < 0 || buffering) return;
    const t = current * WINDOW + v.currentTime;
    reported = t;
    playhead = Math.min(t, duration);
    if (!playing) return;
    if (v.ended || (Number.isFinite(v.duration) && v.currentTime >= v.duration - 0.05)) advance();
  }

  // Playing: follow the video with a timer (animation frames do not run in
  // every embedded browser).
  $effect(() => {
    if (!playing) {
      untrack(() => videos[front]?.pause());
      return;
    }
    untrack(() => {
      if (!ready) {
        playing = false;
        return;
      }
      if (current < 0) seek(playhead);
      else if (!buffering) void videos[front].play().catch(() => (playing = false));
    });
    timer = setInterval(tick, 60);
    return () => clearInterval(timer);
  });

  // A seek from outside (the timeline ruler).
  $effect(() => {
    const t = playhead;
    if (Math.abs(t - reported) < 0.3) return;
    untrack(() => {
      if (ready) seek(t);
      else reported = t;
    });
  });

  // A different project, or edits that change what the windows show.
  $effect(() => {
    const v = `${version}|${ready}`;
    if (v === seen) return;
    seen = v;
    untrack(() => {
      clear();
      error = '';
      if (ready) void show(Math.min(count - 1, Math.floor(playhead / WINDOW)), playhead % WINDOW);
    });
  });

  onDestroy(() => {
    clearInterval(timer);
    clear();
  });

  function stop() {
    playing = false;
    playhead = 0;
  }
</script>

<section class="card preview">
  <div class="screen">
    {#each [0, 1] as i (i)}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video bind:this={videos[i]} class:hidden={front !== i} playsinline></video>
    {/each}
    {#if !ready}
      <div class="empty">
        <b>no preview yet</b>
        <span>open a project from the Say or Song page, or add clips to the timeline</span>
      </div>
    {:else if error}
      <div class="over err">{error}</div>
    {:else if buffering}
      <div class="over">rendering preview…</div>
    {/if}
  </div>
  <div class="transport">
    <button type="button" aria-label="Return to start" title="Return to start" onclick={() => (playhead = 0)}>
      <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M4 4h3v16H4zm15 0v16L8 12z" /></svg>
    </button>
    <button type="button" class="play" aria-pressed={playing} title="Play / pause" onclick={() => (playing = !playing)} disabled={!ready}>{playing ? '❚❚ Pause' : '▶ Play'}</button>
    <button type="button" title="Stop" onclick={stop} disabled={!ready}>■ Stop</button>
    <span class="clock">{clock(playhead)}</span>
    <span class="dur">/ {clock(duration)}</span>
    {#if note}<span class="note">{note}</span>{/if}
  </div>
</section>

<style>
  .preview { padding: .75rem; display: flex; flex-direction: column; gap: .6rem; min-width: 0; }
  .screen { position: relative; aspect-ratio: 16 / 9; width: min(100%, calc(44vh * 16 / 9)); margin: 0 auto; background: #000; border-radius: 8px; border: 1px solid var(--line-2); display: flex; align-items: center; justify-content: center; overflow: hidden; }
  video { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: contain; }
  video.hidden { visibility: hidden; }
  .over { position: absolute; left: .6rem; bottom: .5rem; padding: .15rem .5rem; border-radius: 4px; background: rgba(0, 0, 0, .65); color: var(--dim); font-size: .7rem; }
  .over.err { color: var(--red); }
  .empty { display: flex; flex-direction: column; gap: .4rem; text-align: center; padding: 1rem; color: var(--dim); font-size: .75rem; }
  .empty b { font-family: Impact, 'Arial Black', sans-serif; font-weight: 400; letter-spacing: 1px; font-size: 1.3rem; color: var(--line-2); }
  .transport { display: flex; align-items: center; gap: .4rem; flex-wrap: wrap; }
  .transport button { min-height: 34px; border-radius: 4px; font-size: .76rem; }
  .play { min-width: 96px; color: #0b0b0c; background: var(--green); border-color: var(--green); font-weight: 700; }
  .play:hover:not(:disabled) { background: var(--green); border-color: var(--green); filter: brightness(1.1); }
  .play[aria-pressed='true'] { background: var(--yellow); border-color: var(--yellow); }
  .clock { font-size: 1.05rem; color: var(--yellow); min-width: 6rem; font-variant-numeric: tabular-nums; margin-left: .4rem; }
  .dur { color: var(--dim); font-size: .75rem; }
  .note { color: var(--dim); font-size: .68rem; margin-left: auto; }
</style>
