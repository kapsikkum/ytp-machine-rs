<script lang="ts">
  import { clock } from '#lib/timeline.ts';

  let {
    playhead = $bindable(0),
    duration,
    playing = false,
    loading = false,
    status = '',
    statusError = false,
    onplay,
    onstop
  }: {
    playhead?: number;
    duration: number;
    playing?: boolean;
    loading?: boolean;
    status?: string;
    statusError?: boolean;
    onplay: (range: { from: number; length: number } | null) => void;
    onstop: () => void;
  } = $props();

  let useRange = $state(false);
  let from = $state(0);
  let length = $state('10');

  const jump = (d: number) => (playhead = Math.min(duration, Math.max(0, playhead + d)));
  const toggle = () => {
    if (playing || loading) onstop();
    else onplay(useRange ? { from, length: Number(length) } : null);
  };
</script>

<div class="card wbcard">
  <div class="tools">
    <button type="button" title="Return to song start" aria-label="Return to start" onclick={() => (playhead = 0)}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M4 4h3v16H4zm15 0v16L8 12z" /></svg>
    </button>
    <button type="button" title="Back 10 seconds" aria-label="Rewind 10 seconds" onclick={() => jump(-10)}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M12 4v16L2 12zm10 0v16l-10-8z" /></svg>
    </button>
    <button type="button" class="play" aria-pressed={playing || loading} title="Play the mix from the playhead" onclick={toggle}>
      {loading ? '… mixing' : playing ? '❚❚ Stop' : '▶ Play'}
    </button>
    <button type="button" title="Forward 10 seconds" aria-label="Forward 10 seconds" onclick={() => jump(10)}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M2 4v16l10-8zm10 0v16l10-8z" /></svg>
    </button>
    <span class="clock">{clock(playhead)}</span>
  </div>
  <details class="range">
    <summary class="hint"><span>{useRange ? `${from}s + ${length}s` : 'Rest of the song'}</span> · playback range</summary>
    <div class="preview-range">
      <label><input type="checkbox" bind:checked={useRange} /> Use range</label>
      <label>from <input type="number" bind:value={from} min="0" step="1" /> seconds</label>
      <label>length
        <select bind:value={length}>
          <option value="6">6 seconds</option>
          <option value="10">10 seconds</option>
          <option value="15">15 seconds</option>
          <option value="30">30 seconds</option>
        </select>
      </label>
      <span>Without a range, Play mixes from the playhead to the end. Solo and mute affect playback and the render alike.</span>
    </div>
  </details>
  {#if status}<div class="hint msg" class:err={statusError} role="status">{status}</div>{/if}
</div>

<style>
  .wbcard { width: 100%; margin: 0; border-radius: 0; padding: .55rem 1rem; border-color: var(--line); background: var(--surface); }
  .tools { display: flex; align-items: center; flex-wrap: wrap; gap: .4rem; }
  .tools button { min-height: 40px; min-width: 40px; font-size: .76rem; border-radius: 4px; }
  .play { min-width: 130px; min-height: 44px; padding: .6rem 1rem; color: #0b0b0c; background: var(--green); border-color: var(--green); font-size: .86rem; font-weight: 700; }
  .play:hover:not(:disabled) { background: var(--green); border-color: var(--green); filter: brightness(1.1); }
  .play[aria-pressed='true'] { background: var(--yellow); border-color: var(--yellow); }
  .clock { font-size: 1.05rem; color: var(--yellow); min-width: 6.5rem; font-variant-numeric: tabular-nums; margin-left: .4rem; }
  .range { margin-top: .2rem; }
  .range summary { font-size: .68rem; margin-top: .25rem; cursor: pointer; color: var(--muted); }
  .preview-range { display: flex; align-items: center; flex-wrap: wrap; gap: .6rem; margin-top: .5rem; font-size: .76rem; color: var(--muted); }
  .preview-range input[type='number'] { width: 5rem; }
  .msg { margin-top: .3rem; font-size: .7rem; }
</style>
