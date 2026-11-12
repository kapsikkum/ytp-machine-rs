<script lang="ts">
  import { clock, type Clip, type Track } from '#lib/timeline.ts';

  // The multi-track arrangement view, in the Vegas / Movie Maker manner: pinned
  // track heads on the left, a ruler on top, one lane per track. Styled after
  // the YTPMV workbench (ytpmv-workbench.css) so both pages look the same.
  let {
    tracks,
    duration,
    selected = null,
    playhead = $bindable(0),
    onselect,
    onflag,
    empty = 'No tracks yet.'
  }: {
    tracks: Track[];
    duration: number;
    selected?: string | null;
    playhead?: number;
    onselect?: (id: string) => void;
    onflag?: (id: string, flag: 'mute' | 'solo') => void;
    empty?: string;
  } = $props();

  let pps = $state(60); // pixels per second
  let lanes = $state<HTMLDivElement>();
  let viewWidth = $state(800);
  let scrolled = $state(0);

  const HEAD = 360;
  const width = $derived(Math.max(duration * pps, viewWidth - HEAD));
  const step = $derived.by(() => {
    const target = 90 / pps; // seconds per ~90px
    return [0.25, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300].find((s) => s >= target) ?? 600;
  });
  const ticks = $derived(Array.from({ length: Math.floor(width / pps / step) + 1 }, (_, i) => i * step));

  // A long song is thousands of clips: draw the ones in (or near) the visible stretch.
  function inView(clips: Clip[] | undefined): Clip[] {
    if (!clips || clips.length < 200) return clips ?? [];
    const lo = (scrolled - HEAD) / pps - 2;
    const hi = (scrolled + viewWidth - HEAD) / pps + 2;
    return clips.filter((c) => c.start + c.length >= lo && c.start <= hi);
  }

  function seek(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    const x = e.clientX - el.getBoundingClientRect().left;
    playhead = Math.min(duration, Math.max(0, x / pps));
  }
</script>

<div class="timeline">
  <div class="toolbar">
    <div class="actions">
      <label class="zoom">zoom
        <input type="range" min="10" max="240" step="5" bind:value={pps} aria-label="Timeline zoom" />
        <output>{Math.round(pps)} px/s</output>
      </label>
    </div>
    <div class="readout"><span class="pos">{clock(playhead)}</span><span>{tracks.length} track{tracks.length === 1 ? '' : 's'}</span></div>
  </div>

  <div class="lanes" bind:this={lanes} bind:clientWidth={viewWidth} onscroll={() => (scrolled = lanes?.scrollLeft ?? 0)} role="group" aria-label="Arrangement">
    <div class="content" style:width="{HEAD + width}px" style:--head="{HEAD}px" style:--cursor-x="{HEAD + playhead * pps}px">
      <div class="ruler">
        <span class="corner">tracks</span>
        <div class="time" role="slider" tabindex="0" aria-label="Playhead" aria-valuemin="0" aria-valuemax={duration} aria-valuenow={playhead}
             onpointerdown={seek} onkeydown={(e) => { if (e.key === 'ArrowRight') playhead = Math.min(duration, playhead + 0.5); if (e.key === 'ArrowLeft') playhead = Math.max(0, playhead - 0.5); }}>
          <span class="ticks">
            {#each ticks as t (t)}<span style:left="{t * pps}px">{clock(t).replace(/\.00$/, '')}</span>{/each}
          </span>
        </div>
      </div>

      {#each tracks as t (t.id)}
        <div class="track {t.kind}" class:selected={selected === t.id} class:muted={t.mute} class:solo={t.solo} style:--track-color={t.color}>
          <div class="head">
            <button class="name" type="button" onclick={() => onselect?.(t.id)} aria-pressed={selected === t.id}>
              <b>{t.name}</b>
              {#if t.sub}<small>{t.sub}</small>{/if}
            </button>
            <div class="switches">
              <button type="button" title="Mute" aria-pressed={!!t.mute} onclick={() => onflag?.(t.id, 'mute')}>M</button>
              <button type="button" title="Solo" aria-pressed={!!t.solo} data-flag="solo" onclick={() => onflag?.(t.id, 'solo')}>S</button>
            </div>
            <span class="kind">{t.kind}</span>
          </div>
          <div class="region" role="presentation" onpointerdown={seek}>
            {#each inView(t.clips) as c (c.id)}
              <button type="button" class="clip" style:left="{c.start * pps}px" style:width="{Math.max(6, c.length * pps)}px" title={c.label}
                      onpointerdown={(e) => e.stopPropagation()} onclick={() => onselect?.(t.id)}>{c.label}</button>
            {/each}
            {#if t.notes?.length}
              <svg viewBox="0 0 {width} 100" preserveAspectRatio="none" aria-hidden="true">
                {#each t.notes as n, i (i)}
                  <rect x={n.t * pps} y={92 - n.p * 84} width={Math.max(2, n.d * pps)} height="8" />
                {/each}
              </svg>
            {/if}
          </div>
        </div>
      {:else}
        <div class="none">{empty}</div>
      {/each}

      <div class="cursor"></div>
    </div>
  </div>
</div>

<style>
  .timeline { background: var(--surface); border: 1px solid var(--line); }
  .toolbar { display: flex; justify-content: space-between; align-items: center; flex-wrap: wrap; gap: .4rem 1rem; padding: .5rem .7rem; border-bottom: 1px solid var(--line-2); }
  .actions { display: flex; align-items: center; gap: .35rem; flex-wrap: wrap; }
  .zoom { display: flex; align-items: center; gap: .35rem; font-size: .65rem; color: var(--muted); }
  .zoom input { width: 90px; min-width: 0; }
  .zoom output { font-size: .64rem; color: var(--muted); min-width: 3.7rem; }
  .readout { display: flex; gap: .8rem; flex-wrap: wrap; align-items: center; font-size: .65rem; color: var(--muted); }
  .pos { color: var(--yellow); font-variant-numeric: tabular-nums; min-width: 5.5rem; }

  .lanes {
    --track-height: 78px; width: 100%; max-height: clamp(240px, 48vh, 560px); overflow: auto; overscroll-behavior: contain;
    scrollbar-gutter: stable; position: relative; min-height: 130px; resize: vertical;
  }
  .content { position: relative; min-width: 100%; isolation: isolate; }

  .ruler, .track { display: grid; grid-template-columns: var(--head) minmax(0, 1fr); border-bottom: 1px solid var(--line-2); }
  .ruler { position: sticky; top: 0; z-index: 10; background: var(--raised); height: 31px; font-size: .66rem; color: var(--muted); }
  .corner { position: sticky; left: 0; z-index: 12; padding: .45rem .75rem; background: var(--raised); border-right: 1px solid var(--line-2); }
  .time { position: relative; overflow: hidden; cursor: crosshair; touch-action: none; background: var(--raised); }
  .ticks { display: block; position: absolute; inset: 0; pointer-events: none; }
  .ticks > span { position: absolute; top: 0; height: 100%; padding: 6px 4px; border-left: 1px solid var(--line-2); white-space: nowrap; }

  .track { height: var(--track-height); min-height: var(--track-height); }
  .head {
    position: sticky; left: 0; z-index: 9; height: var(--track-height); min-width: 0; overflow: hidden;
    display: grid; grid-template-columns: minmax(0, 1fr) auto; grid-template-areas: 'name switches' 'kind kind';
    gap: .35rem .5rem; padding: .4rem .6rem; align-content: center;
    border-left: 3px solid var(--track-color); border-right: 1px solid var(--line-2); background: var(--surface);
  }
  .track.selected .head { background: var(--raised); }
  .name {
    grid-area: name; display: flex; flex-direction: column; gap: .2rem; min-width: 0; overflow: hidden; color: var(--text);
    text-align: left; padding: 0; border: 0; border-radius: 0; background: transparent; white-space: normal; align-items: flex-start;
  }
  .name:hover:not(:disabled) { background: transparent; }
  .name b { font-size: .74rem; font-weight: 500; display: block; width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .name small { color: var(--muted); font-size: .64rem; display: block; width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .track.selected .name b { color: var(--yellow); }
  .switches { grid-area: switches; display: flex; gap: .2rem; }
  .switches button { min-width: 22px; font-size: .65rem; padding: .15rem .2rem; min-height: 24px; border-radius: 3px; }
  .switches button[aria-pressed='true'] { color: var(--yellow); border-color: var(--yellow); }
  .track.solo .switches button[data-flag='solo'] { color: var(--green); border-color: var(--green); }
  .kind { grid-area: kind; color: var(--track-color); font-size: .6rem; letter-spacing: 1.2px; text-transform: uppercase; }

  .region {
    --grid-line: var(--line); position: relative; min-width: 0; margin: 0; overflow: hidden; cursor: crosshair; touch-action: pan-y;
    background: repeating-linear-gradient(90deg, transparent 0, transparent calc(12.5% - 1px), var(--grid-line) calc(12.5% - 1px), var(--grid-line) 12.5%), var(--bg);
    color: var(--track-color);
  }
  .track.selected .region {
    background: repeating-linear-gradient(90deg, transparent 0, transparent calc(12.5% - 1px), var(--grid-line) calc(12.5% - 1px), var(--grid-line) 12.5%), var(--surface);
  }
  .track.muted .region { opacity: .38; }
  .clip {
    position: absolute; top: 12px; bottom: 12px; padding: 0 .4rem; justify-content: flex-start; overflow: hidden;
    text-overflow: ellipsis; font-size: .62rem; border-radius: 4px; color: var(--text);
    background: color-mix(in srgb, var(--track-color) 22%, var(--surface)); border: 1px solid var(--track-color);
  }
  .clip:hover:not(:disabled) { background: color-mix(in srgb, var(--track-color) 34%, var(--surface)); border-color: var(--track-color); }
  .region svg { position: absolute; inset: 0; width: 100%; height: 100%; display: block; background: color-mix(in srgb, var(--track-color) 8%, transparent); }
  .region rect { fill: currentColor; stroke: currentColor; stroke-width: .5px; }

  .cursor { position: absolute; top: 0; bottom: 0; left: var(--cursor-x); width: 1px; background: var(--yellow); z-index: 8; pointer-events: none; }
  .none { padding: 1.5rem; font-size: .8rem; color: var(--muted); }

  @media (max-width: 650px) {
    .lanes { --track-height: 96px; max-height: 52vh; }
  }
</style>
