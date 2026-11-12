<script lang="ts">
  import { LAYOUTS, type Part } from '#lib/ytpmv/model.ts';

  let {
    parts,
    layout = $bindable('balanced'),
    fit = $bindable('contain'),
    center = $bindable(true)
  }: { parts: Part[]; layout?: string; fit?: string; center?: boolean } = $props();

  interface Box { x: number; y: number; w: number; h: number }

  // Where each tile goes in the 16:9 frame, as fractions.
  const boxes = $derived.by<Box[]>(() => {
    const n = parts.length;
    if (n === 0) return [];
    const grid = (cols: number, rows: number): Box[] =>
      parts.map((_, i) => ({ x: (i % cols) / cols, y: Math.floor(i / cols) / rows, w: 1 / cols, h: 1 / rows }));
    switch (layout) {
      case 'two-rows': return grid(Math.ceil(n / 2), 2);
      case 'two-columns': return grid(2, Math.ceil(n / 2));
      case 'strips': return grid(n, 1);
      case 'featured':
        return parts.map((_, i) =>
          i === 0 ? { x: 0, y: 0, w: 0.7, h: 1 } : { x: 0.7, y: (i - 1) / (n - 1 || 1), w: 0.3, h: 1 / (n - 1 || 1) }
        );
      default: {
        const cols = Math.ceil(Math.sqrt(n));
        return grid(cols, Math.ceil(n / cols));
      }
    }
  });
</script>

<div class="panel-video">
  <div class="heading">
    <b>Video composition</b>
    <span>16:9 · {fit === 'contain' ? 'whole clips' : 'cropped to fill'}</span>
    <div class="controls">
      <label>Grid
        <select bind:value={layout} aria-label="Video grid preset">
          {#each LAYOUTS as [id, label] (id)}<option value={id}>{label}</option>{/each}
        </select>
      </label>
      <label>Framing
        <select bind:value={fit} aria-label="Media framing" title="Fit shows the whole clip, centered with black bars where needed. Fill crops the edges to cover its tile.">
          <option value="contain">Fit whole clip</option>
          <option value="cover">Fill tile (crop)</option>
        </select>
      </label>
      <label title="Center the classic grid and incomplete rows. Turn off to align them top-left."><input type="checkbox" bind:checked={center} /> Center tiles</label>
    </div>
  </div>
  <div class="preview" data-media-fit={fit}>
    {#each parts as p, i (p.id)}
      {@const b = boxes[i]}
      {#if b}
        <div class="tile" style:left="{b.x * 100}%" style:top="{b.y * 100}%" style:width="{b.w * 100}%" style:height="{b.h * 100}%">
          <small>{i + 1}</small>
          <b>{p.text || p.name}</b>
          <span>{p.name}</span>
        </div>
      {/if}
    {:else}
      <div class="empty">Load a song to see its tiles.</div>
    {/each}
  </div>
  <p class="status" role="status">Tiles follow the order of the tracks</p>
</div>

<style>
  .panel-video { padding: 1rem; }
  .heading { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: .5rem; color: var(--muted); font-size: .72rem; margin-bottom: .6rem; }
  .heading b { font-weight: 500; color: var(--text); }
  .controls { display: flex; align-items: center; flex-wrap: wrap; gap: .6rem 1rem; }
  .controls label { display: flex; align-items: center; gap: .5rem; }
  .controls select { font-size: .72rem; }
  .preview { aspect-ratio: 16 / 9; width: min(100%, 1000px); margin: auto; background: var(--bg); position: relative; border: 1px solid var(--line-2); }
  .tile {
    position: absolute; display: flex; flex-direction: column; gap: .4rem; border: 1px solid var(--bg); background: var(--raised);
    overflow: hidden; align-items: center; justify-content: center; cursor: grab; transition: left .22s var(--ease), top .22s var(--ease), width .22s var(--ease), height .22s var(--ease);
  }
  .tile b { font-size: clamp(.7rem, 2vw, 1.4rem); font-weight: 500; color: var(--yellow); overflow-wrap: anywhere; }
  .tile span { font-size: .65rem; color: var(--muted); }
  .tile small { position: absolute; top: .4rem; left: .5rem; font-size: .6rem; color: var(--muted); }
  .empty { margin: auto; color: var(--muted); font-size: .75rem; padding: 1rem; }
  .status { color: var(--muted); font-size: .66rem; margin: .7rem 0; }
  @media (prefers-reduced-motion: reduce) { .tile { transition: none; } }
</style>
