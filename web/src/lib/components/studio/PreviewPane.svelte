<script lang="ts">
  import { clock } from '#lib/timeline.ts';

  let {
    playhead = $bindable(0),
    duration,
    playing = $bindable(false),
    src = ''
  }: { playhead?: number; duration: number; playing?: boolean; src?: string } = $props();

  function stop() {
    playing = false;
    playhead = 0;
  }
</script>

<section class="card preview">
  <div class="screen">
    {#if src}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video {src} playsinline></video>
    {:else}
      <div class="empty">
        <b>no preview yet</b>
        <span>generate something with the prompt box, or add clips to the timeline</span>
      </div>
    {/if}
  </div>
  <div class="transport">
    <button type="button" aria-label="Return to start" title="Return to start" onclick={() => (playhead = 0)}>
      <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M4 4h3v16H4zm15 0v16L8 12z" /></svg>
    </button>
    <button type="button" class="play" aria-pressed={playing} title="Play / pause" onclick={() => (playing = !playing)} disabled={!src}>{playing ? '❚❚ Pause' : '▶ Play'}</button>
    <button type="button" title="Stop" onclick={stop} disabled={!src}>■ Stop</button>
    <span class="clock">{clock(playhead)}</span>
    <span class="dur">/ {clock(duration)}</span>
  </div>
</section>

<style>
  .preview { padding: .75rem; display: flex; flex-direction: column; gap: .6rem; min-width: 0; }
  .screen { aspect-ratio: 16 / 9; width: min(100%, calc(44vh * 16 / 9)); margin: 0 auto; background: #000; border-radius: 8px; border: 1px solid var(--line-2); display: flex; align-items: center; justify-content: center; overflow: hidden; }
  video { width: 100%; height: 100%; object-fit: contain; }
  .empty { display: flex; flex-direction: column; gap: .4rem; text-align: center; padding: 1rem; color: var(--dim); font-size: .75rem; }
  .empty b { font-family: Impact, 'Arial Black', sans-serif; font-weight: 400; letter-spacing: 1px; font-size: 1.3rem; color: var(--line-2); }
  .transport { display: flex; align-items: center; gap: .4rem; flex-wrap: wrap; }
  .transport button { min-height: 34px; border-radius: 4px; font-size: .76rem; }
  .play { min-width: 96px; color: #0b0b0c; background: var(--green); border-color: var(--green); font-weight: 700; }
  .play:hover:not(:disabled) { background: var(--green); border-color: var(--green); filter: brightness(1.1); }
  .play[aria-pressed='true'] { background: var(--yellow); border-color: var(--yellow); }
  .clock { font-size: 1.05rem; color: var(--yellow); min-width: 6rem; font-variant-numeric: tabular-nums; margin-left: .4rem; }
  .dur { color: var(--dim); font-size: .75rem; }
</style>
