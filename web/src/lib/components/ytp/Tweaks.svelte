<script lang="ts">
  import { DEFAULTS, PRESETS, SLIDERS, TOGGLES, matchesPreset, withDefaults, type Options } from '#lib/ytp/options.ts';

  let { opts = $bindable(), open }: { opts: Options; open: boolean } = $props();

  const tuned = $derived(!!opts.sing || !!opts.vocode);
  const num = (k: string) => Number(opts[k]);
  const pct = (min: number, max: number, v: number) => `${((v - min) / (max - min)) * 100}%`;
</script>

{#snippet slider(o: (typeof SLIDERS)[number])}
  <label class="slider" title={o.hint}>
    <span class="name">{o.name}</span><output>{o.fmt(num(o.key))}</output>
    <input
      type="range"
      min={o.min}
      max={o.max}
      step={o.step}
      value={num(o.key)}
      style:--p={pct(o.min, o.max, num(o.key))}
      oninput={(e) => (opts = { ...opts, [o.key]: Number(e.currentTarget.value) })}
    />
  </label>
{/snippet}

<div class="tweaks" class:open id="tweaks">
  <div class="inner">
    <div class="body">
      <div class="presets" role="group" aria-label="Presets">
        {#each PRESETS as p (p.name)}
          <button type="button" class:on={matchesPreset(opts, p)} onclick={() => (opts = withDefaults(p.opts))}>{p.name}</button>
        {/each}
      </div>
      <div class="sliders">
        {#each SLIDERS.filter((o) => !o.sing) as o (o.key)}{@render slider(o)}{/each}
      </div>
      <div class="toggles">
        {#each TOGGLES as o (o.key)}
          <label class="switch" title={o.hint}>
            <input type="checkbox" checked={!!opts[o.key]} onchange={(e) => (opts = { ...opts, [o.key]: e.currentTarget.checked })} /><span class="track"></span>
            {o.name}
          </label>
        {/each}
      </div>
      {#if tuned}
        <div class="sing-panel">
          <div class="section-label" style="margin:0">the tune</div>
          <div class="sliders">
            {#each SLIDERS.filter((o) => o.sing) as o (o.key)}{@render slider(o)}{/each}
          </div>
          <div class="sing-midi">
            <label title="Sing to the top line of a MIDI file instead of a tune shape, one note a word">tune from a MIDI
              <input type="file" accept=".mid,.midi,audio/midi" disabled />
            </label>
            <span class="hint">not available yet</span>
          </div>
          <div class="hint"><b>Click a word chip</b> to move that word up or down from the tune.</div>
        </div>
      {/if}
      <div class="voice-settings">
        <span>for this voice</span>
        <label title="How hard to try for a word the voice never said">effort
          <select disabled>
            <option>strict — real clips only</option>
            <option>loose — near-enough sounds</option>
            <option>desperate — say it regardless</option>
          </select>
        </label>
        <span class="hint">not available yet</span>
        <button type="button" class="linkish" onclick={() => (opts = { ...DEFAULTS })}>reset tweaks</button>
      </div>
    </div>
  </div>
</div>

<style>
  .tweaks { display: grid; grid-template-rows: 0fr; transition: grid-template-rows .28s var(--ease); }
  .tweaks.open { grid-template-rows: 1fr; }
  .tweaks > .inner { overflow: hidden; min-height: 0; }
  .tweaks .body { padding: 1rem .35rem .25rem; display: flex; flex-direction: column; gap: 1rem; }
  .presets { display: flex; flex-wrap: wrap; gap: .35rem; }
  .presets button {
    cursor: pointer; background: var(--raised); border: 1px solid var(--line-2); border-radius: 999px;
    padding: .32rem .75rem; font-size: .75rem; color: var(--muted); transition: all .15s;
  }
  .presets button:hover { color: var(--text); border-color: #4a4a52; background: var(--raised); }
  .presets button.on { color: #000; background: var(--yellow); border-color: var(--yellow); }
  .sliders { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: .9rem 1.4rem; }
  .slider { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: .35rem .5rem; align-items: center; }
  .slider .name { color: var(--muted); font-size: .75rem; white-space: nowrap; }
  .slider output { color: var(--text); font-size: .75rem; font-variant-numeric: tabular-nums; text-align: right; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .slider input { grid-column: 1 / -1; }
  .toggles { display: flex; flex-wrap: wrap; gap: .6rem 1.4rem; }
  .sing-panel { border-top: 1px dashed var(--line); padding-top: .9rem; display: flex; flex-direction: column; gap: .9rem; }
  .sing-midi { display: flex; flex-wrap: wrap; align-items: center; gap: .5rem .8rem; color: var(--muted); font-size: .75rem; }
  .voice-settings {
    display: flex; flex-wrap: wrap; align-items: center; gap: .5rem .9rem;
    border-top: 1px dashed var(--line); padding-top: .9rem; color: var(--dim); font-size: .72rem;
  }
  .voice-settings label { display: inline-flex; align-items: center; gap: .45rem; color: var(--muted); font-size: .75rem; }
  .voice-settings .linkish { margin-left: auto; }
</style>
