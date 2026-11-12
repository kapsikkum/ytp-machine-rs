<script lang="ts" module>
  export interface RenderOptions {
    len: string;
    speed: string;
    transpose: string;
    vary: string;
    balance: boolean;
    accurate: boolean;
    jitter: boolean;
    flip: boolean;
    flash: boolean;
    dim: boolean;
    labels: boolean;
    draft: boolean;
    /** no voice: every part is played by a chip, and no corpus is needed */
    synth: boolean;
  }
</script>

<script lang="ts">
  let { o = $bindable(), onresetparts }: { o: RenderOptions; onresetparts: () => void } = $props();

  const transposes = Array.from({ length: 25 }, (_, i) => i - 12);
</script>

<div class="panel-song">
  <div id="options">
    <div class="group"><span class="gl">song</span>
      <label>length
        <select bind:value={o.len}>
          <option value="30">first 30s</option>
          <option value="60">first minute</option>
          <option value="0">whole song</option>
        </select>
      </label>
      <label>speed
        <select bind:value={o.speed}>
          <option value="0.75">slower</option>
          <option value="1">as written</option>
          <option value="1.25">faster</option>
          <option value="1.5">much faster</option>
        </select>
      </label>
      <label title="Move the whole song up or down, in semitones">key
        <select bind:value={o.transpose}>
          {#each transposes as t (t)}<option value={String(t)}>{t > 0 ? '+' : ''}{t}</option>{/each}
        </select>
      </label>
    </div>
    <div class="group"><span class="gl">sound</span>
      <label title="One recording on every beat sounds like a loop. Rotating plays the best few takes of the same word; ultra picks a different word for every hit, each still sung on its note.">variety
        <select bind:value={o.vary}>
          <option value="off">one take only</option>
          <option value="rotate">rotate takes</option>
          <option value="random">random takes</option>
          <option value="ultra">ultra: different words</option>
        </select>
      </label>
      <label title="Measure how loud each part actually came out and move it to where its job wants it, rather than trusting a fixed table that cannot tell a whisper from a brass patch. Then set the level of the finished mix."><input type="checkbox" bind:checked={o.balance} /> auto balance</label>
      <label title="Only as many notes at once as the chip really had channels for. When a song asks for more, a new note takes a channel over and cuts off what was on it, as the drivers did."><input type="checkbox" bind:checked={o.accurate} /> accurate channels</label>
      <label title="Leave the voice out: every part is played by the chip, and each tile shows its oscilloscope. Needs no corpus."><input type="checkbox" bind:checked={o.synth} /> chip only (no voice)</label>
      <label title="A hair of level on every hit, and a hair of tuning on drums, so a part sounds played rather than triggered"><input type="checkbox" bind:checked={o.jitter} /> jitter</label>
    </div>
    <div class="group"><span class="gl">picture</span>
      <label title="Mirror a tile on every other note, the classic YTPMV look"><input type="checkbox" bind:checked={o.flip} /> flip</label>
      <label title="A flash of brightness on every hit"><input type="checkbox" bind:checked={o.flash} /> flash</label>
      <label title="Darken a tile when its part is not playing"><input type="checkbox" bind:checked={o.dim} /> dim idle</label>
      <label title="Write each part's word on its tile, or what plays it when there is no voice"><input type="checkbox" bind:checked={o.labels} /> labels</label>
      <label title="Lower-resolution tiles, so the render is quicker"><input type="checkbox" bind:checked={o.draft} /> draft</label>
    </div>
  </div>
  <details class="bulk">
    <summary>change multiple parts</summary>
    <div class="tools">
      <button type="button" onclick={onresetparts} title="Restore each part's suggested settings">reset to suggestions</button>
    </div>
  </details>
</div>

<style>
  .panel-song { padding: 1rem; }
  #options { display: flex; flex-direction: column; gap: 1rem; color: var(--muted); font-size: .78rem; }
  .group { display: flex; flex-wrap: wrap; gap: .5rem 1.1rem; align-items: center; }
  .gl { color: var(--dim); font-size: .66rem; letter-spacing: 1.2px; text-transform: uppercase; min-width: 4.2rem; }
  label { display: inline-flex; gap: .4rem; align-items: center; }
  .bulk { margin: 1rem 0 0; padding-top: .65rem; border-top: 1px solid var(--line); font-size: .78rem; }
  .bulk summary { cursor: pointer; }
  .tools { display: flex; flex-wrap: wrap; gap: .5rem; margin-top: .5rem; }
  .tools button { border-radius: 999px; }
</style>
