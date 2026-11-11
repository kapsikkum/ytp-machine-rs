<script lang="ts">
  import type { Song } from '#lib/ytpmv/model.ts';
  import { clock } from '#lib/timeline.ts';

  let {
    song,
    canRender = true,
    rendering = false,
    onimport,
    onrender
  }: { song: Song; canRender?: boolean; rendering?: boolean; onimport: () => void; onrender: () => void } = $props();
</script>

<div class="card wbcard">
  <h2>{song.title}</h2>
  <div class="chips">
    <span class="chip"><b>{song.bpm}</b> bpm</span>
    <span class="chip"><b>{song.key}</b></span>
    <span class="chip"><b>{song.parts.length}</b> parts</span>
    <span class="chip"><b>{clock(song.duration).replace(/\.00$/, '')}</b> long</span>
    {#if song.sample}<span class="chip sample">sample · layout only</span>{/if}
  </div>
  <div class="project-tools">
    <button type="button" onclick={onimport}>Import MIDI</button>
    {#if canRender}<button type="button" class="primary" disabled={rendering} onclick={onrender}>render video</button>{/if}
  </div>
</div>

<style>
  .wbcard {
    width: 100%; margin: 1rem 0 0; border-radius: 0; padding: .7rem 1rem; background: var(--surface);
    display: flex; flex-wrap: wrap; align-items: center; gap: .55rem 1rem;
  }
  h2 { font-size: 1rem; letter-spacing: 0; color: var(--text); font-weight: 500; }
  .chips { flex: 1; display: flex; flex-wrap: wrap; gap: .7rem; }
  .chip { font-size: .68rem; color: var(--muted); white-space: nowrap; }
  .chip b { color: var(--text); font-weight: 400; }
  .chip.sample { color: var(--amber); }
  .project-tools { display: flex; gap: .5rem; flex-wrap: wrap; align-items: center; }
  .project-tools button { font-size: .75rem; min-height: 34px; border-radius: 4px; }
  @media (max-width: 650px) {
    .chips { flex-basis: 100%; order: 3; }
    .project-tools { margin-left: auto; }
  }
</style>
