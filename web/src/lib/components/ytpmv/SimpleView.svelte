<script lang="ts">
  import { corpora } from '#lib/voice.ts';
  import type { MidiAnalysis } from '#lib/types.ts';
  import type { Part } from '#lib/ytpmv/model.ts';

  // The Simple interface: the song as a plain list of parts, one row each.
  // Same state as the Advanced view, without the timeline.
  let {
    parts,
    analysis,
    defaultVoice,
    busy = '',
    onplaypart,
    onplayword,
    onvoice
  }: {
    parts: Part[];
    analysis: MidiAnalysis;
    defaultVoice: string;
    busy?: string;
    onplaypart: (p: Part) => void;
    onplayword: (p: Part) => void;
    onvoice: (p: Part) => void;
  } = $props();

  const ins = $derived(analysis.instruments);
  const words = $derived([...Object.keys(analysis.units), ...Object.keys(analysis.noises)]);
</script>

<div class="simple-parts">
  <datalist id="ytpmv-simple-words">{#each words as w (w)}<option value={w}></option>{/each}</datalist>
  {#each parts as p (p.id)}
    <div class="part" class:muted={p.mute}>
      <div class="who">
        <div class="role">{p.role}</div>
        <div class="name">{p.name}</div>
        <div class="meta">{p.program} · {p.noteCount.toLocaleString()} notes</div>
      </div>
      <div class="sound">
        <div class="row">
          <input type="text" list="ytpmv-simple-words" maxlength="60" bind:value={p.text} placeholder="a word or phrase" aria-label="Word for {p.name}" />
          <button type="button" class="play" disabled={busy !== '' || !p.text.trim()} onclick={() => onplayword(p)} title="Hear the clip">▶</button>
          <button type="button" aria-label="Previous take" disabled={p.take === 0} onclick={() => (p.take -= 1)}>‹</button>
          <span class="take">take {p.take + 1}</span>
          <button type="button" aria-label="Next take" onclick={() => (p.take += 1)}>›</button>
        </div>
        {#if p.alternatives.length}
          <div class="row alts">
            {#each p.alternatives.slice(0, 6) as a (a.text + ':' + a.take)}
              <button type="button" class="alt" class:on={a.text === p.text && a.take === p.take} onclick={() => { p.text = a.text; p.take = a.take; }}>{a.text}{a.take ? ` · ${a.take + 1}` : ''}</button>
            {/each}
          </div>
        {/if}
      </div>
      <div class="knobs">
        <div class="row">
          <select bind:value={p.corpus} aria-label="Voice for {p.name}" onchange={() => onvoice(p)}>
            <option value="">voice: {defaultVoice || 'default'}</option>
            {#each $corpora as c (c.name)}<option value={c.name}>{c.name}</option>{/each}
          </select>
          <select bind:value={p.tone} aria-label="Instrument for {p.name}">
            <option value="">instrument: auto</option>
            <optgroup label="the voice">
              {#each ins.voice as [name, about] (name)}<option value={name} title={about}>{name}</option>{/each}
            </optgroup>
            {#each Object.entries(ins.machines) as [key, m] (key)}
              {@const tones = p.isDrums ? m.drums : m.melodic}
              {#if tones.length}
                <optgroup label={m.name}>
                  {#each tones as [name, about] (name)}<option value={name} title={about}>{name}</option>{/each}
                </optgroup>
              {/if}
            {/each}
          </select>
        </div>
        <div class="row">
          <label class="mix">level <input type="range" min="0" max="200" bind:value={p.volume} aria-label="Level of {p.name}" /></label>
          <label><input type="checkbox" bind:checked={p.mute} /> mute</label>
          <button type="button" class="play" disabled={busy !== ''} onclick={() => onplaypart(p)}>▶ part</button>
        </div>
      </div>
    </div>
  {/each}
</div>

<style>
  .simple-parts { display: flex; flex-direction: column; gap: .6rem; padding: 1rem; background: var(--bg); }
  .part {
    display: grid; grid-template-columns: minmax(150px, .9fr) minmax(250px, 1.6fr) minmax(270px, 1.4fr); gap: .75rem 1rem; align-items: start;
    border: 1px solid var(--line); border-radius: 10px; padding: .85rem .95rem; background: var(--surface);
  }
  .part.muted { opacity: .5; }
  .role { font-size: .66rem; letter-spacing: 1.2px; text-transform: uppercase; color: var(--yellow); }
  .name { color: var(--text); margin: .15rem 0; }
  .meta { color: var(--dim); font-size: .72rem; line-height: 1.5; }
  .row { display: flex; align-items: center; gap: .4rem; flex-wrap: wrap; }
  .row + .row { margin-top: .45rem; }
  .row input[type='text'] { flex: 1; min-width: 7rem; }
  .take { color: var(--muted); font-size: .74rem; }
  .play { color: var(--green); }
  .alt { border-radius: 999px; padding: .1rem .6rem; min-height: 28px; font-size: .7rem; }
  .alt.on { border-color: var(--yellow); color: var(--yellow); }
  .knobs select { flex: 1; min-width: 8rem; max-width: 100%; }
  .mix { display: inline-flex; align-items: center; gap: .4rem; color: var(--muted); font-size: .72rem; }
  .mix input { max-width: 7rem; }
  label { color: var(--muted); font-size: .72rem; display: inline-flex; align-items: center; gap: .3rem; }
  .part select, .part input, .part button { font-size: .74rem; }
  @media (max-width: 800px) {
    .part { grid-template-columns: minmax(0, 1fr); }
  }
</style>
