<script lang="ts">
  import { corpora } from '#lib/voice.ts';
  import type { MidiAnalysis } from '#lib/types.ts';
  import type { Part } from '#lib/ytpmv/model.ts';

  let {
    part = $bindable(),
    analysis,
    defaultVoice,
    chip = '',
    busy = '',
    onplaypart,
    onplayword,
    onvoice
  }: {
    part: Part | undefined;
    analysis: MidiAnalysis;
    defaultVoice: string;
    chip?: string;
    /** what is being fetched for this track ('' for nothing) */
    busy?: string;
    onplaypart: () => void;
    onplayword: () => void;
    onvoice: () => void;
  } = $props();

  const ins = $derived(analysis.instruments);
  const synth = $derived(!!part && part.tone !== '' && ins.synth.includes(part.tone));
  const words = $derived([...Object.keys(analysis.units), ...Object.keys(analysis.noises)]);
  const autoTone = $derived.by(() => {
    const a = analysis.parts.find((p) => p.id === part?.id);
    return a && chip ? (a.chip_tones[chip] ?? '') : '';
  });
</script>

{#if part}
  <div class="part" class:instrumental={synth}>
    <div class="who">
      <div class="role">{part.role}</div>
      <div class="name">{part.name}</div>
      <div class="meta">{part.program} · {part.noteCount.toLocaleString()} notes</div>
      <button type="button" class="play" disabled={busy !== ''} onclick={onplaypart} title="Hear this track alone, as it will be in the render">
        {busy === 'part' ? '… mixing' : '▶ part'}
      </button>
    </div>

    {#if !synth}
      <div class="sound">
        <div class="row">
          <input type="text" list="ytpmv-words" maxlength="60" bind:value={part.text} placeholder="a word or phrase" aria-label="Word for this part" />
          <datalist id="ytpmv-words">{#each words as w (w)}<option value={w}></option>{/each}</datalist>
        </div>
        <div class="row">
          <button type="button" class="play" disabled={busy !== '' || !part.text.trim()} onclick={onplayword} title="Hear the clip for this word">
            {busy === 'word' ? '…' : '▶'}
          </button>
          <button type="button" aria-label="Previous take" disabled={part.take === 0} onclick={() => (part.take -= 1)}>‹</button>
          <span class="take">take {part.take + 1}</span>
          <button type="button" aria-label="Next take" onclick={() => (part.take += 1)}>›</button>
          {#if part.score !== null && part.text === part.base.text && part.take === part.base.take}
            <span class="badge" title="How well this clip fits the notes of the part">fit {Math.round(part.score * 100) / 100}</span>
          {:else}
            <span class="badge">chosen by hand</span>
          {/if}
        </div>
        <div class="alts">
          {#each part.alternatives as a (a.text + ':' + a.take)}
            <button
              type="button"
              class="alt"
              class:on={a.text === part.text && a.take === part.take}
              title="score {a.score.toFixed(2)}"
              onclick={() => { part.text = a.text; part.take = a.take; }}
            >{a.text}{a.take ? ` · ${a.take + 1}` : ''}</button>
          {:else}
            <span class="or">{analysis.corpus ? 'no other words suit this part' : 'install a voice to get word suggestions'}</span>
          {/each}
        </div>
      </div>
    {/if}

    <div class="knobs">
      <div class="kv">
        <div class="kvrow">
          <span class="k">voice</span>
          <span class="v">
            <select bind:value={part.corpus} disabled={synth} onchange={onvoice}>
              <option value="">song default{defaultVoice ? ` (${defaultVoice})` : ''}</option>
              {#each $corpora as c (c.name)}<option value={c.name}>{c.name}</option>{/each}
            </select>
          </span>
        </div>
        <div class="kvrow">
          <span class="k">instrument</span>
          <span class="v">
            <select bind:value={part.tone} aria-label="Instrument">
              <option value="">auto{autoTone ? ` — ${autoTone}` : chip ? '' : ' — the voice'}</option>
              <optgroup label="the voice">
                {#each ins.voice as [name, about] (name)}<option value={name} title={about}>{name}</option>{/each}
              </optgroup>
              {#each Object.entries(ins.machines) as [key, m] (key)}
                {@const tones = part.isDrums ? m.drums : m.melodic}
                {#if tones.length}
                  <optgroup label={m.name}>
                    {#each tones as [name, about] (name)}<option value={name} title={about}>{name}</option>{/each}
                  </optgroup>
                {/if}
              {/each}
            </select>
          </span>
        </div>
        <div class="kvrow">
          <span class="k">octave</span>
          <span class="v oct">
            <button type="button" disabled={part.octave <= -4} onclick={() => (part.octave -= 1)} aria-label="Octave down">−</button>
            <span>{part.octave > 0 ? '+' : ''}{part.octave}</span>
            <button type="button" disabled={part.octave >= 4} onclick={() => (part.octave += 1)} aria-label="Octave up">+</button>
          </span>
        </div>
        <div class="kvrow">
          <span class="k">mix</span>
          <span class="v mix">
            <input type="range" min="0" max="200" step="1" bind:value={part.volume} aria-label="Part volume" />
            <input type="number" min="0" max="200" bind:value={part.volume} aria-label="Volume percent" />
            <span class="tiny">%</span>
          </span>
        </div>
      </div>
      <div class="flags">
        <label><input type="checkbox" bind:checked={part.mute} /> mute</label>
        <label><input type="checkbox" bind:checked={part.solo} /> solo</label>
      </div>
    </div>
  </div>
{:else}
  <div class="none">Select a track to edit its voice or chip instrument.</div>
{/if}

<style>
  .none { padding: 1.5rem; font-size: .8rem; color: var(--muted); }
  .part {
    display: grid; grid-template-columns: minmax(220px, 1fr) minmax(290px, 1fr); gap: 1rem 1.5rem; align-items: start;
    background: var(--surface); padding: 1rem;
  }
  .part.instrumental .knobs { grid-column: 1 / -1; max-width: 800px; }
  .who { grid-column: 1 / -1; display: flex; align-items: baseline; gap: .4rem .8rem; flex-wrap: wrap; padding-bottom: .7rem; border-bottom: 1px solid var(--line); }
  .who .role { font-size: .66rem; letter-spacing: 1.2px; text-transform: uppercase; color: var(--yellow); }
  .who .name { color: var(--text); font-size: .85rem; }
  .who .meta { color: var(--dim); font-size: .67rem; line-height: 1.5; }
  .who .play { margin-left: auto; }
  button.play { color: var(--green); }
  .sound .row { display: flex; align-items: center; gap: .3rem; flex-wrap: wrap; }
  .sound .row + .row { margin-top: .45rem; }
  .sound input[type='text'] { flex: 1 0 100%; min-width: 7rem; }
  .take { color: var(--muted); font-size: .74rem; min-width: 6rem; text-align: center; }
  .badge { font-size: .7rem; border-radius: 999px; padding: .15rem .5rem; background: var(--raised); color: var(--muted); border: 1px solid var(--line); }
  .alts { margin-top: .5rem; display: flex; flex-wrap: wrap; gap: .3rem; }
  .alt { border-radius: 999px; padding: .1rem .6rem; min-height: 28px; }
  .alt.on { border-color: var(--yellow); color: var(--yellow); }
  .or { color: var(--dim); font-size: .7rem; }
  .part select, .part input, .part button { font-size: .72rem; border-radius: 4px; min-height: 34px; }
  .part .alt { border-radius: 999px; min-height: 28px; }
  .knobs .kv { display: grid; grid-template-columns: 60px minmax(0, 1fr); gap: .4rem .6rem; align-items: center; }
  .knobs .kvrow { display: contents; }
  .knobs .k { color: var(--dim); font-size: .7rem; text-align: right; white-space: nowrap; }
  .knobs .v { display: flex; align-items: center; gap: .4rem; min-width: 0; }
  .knobs .v select { width: 100%; white-space: normal; }
  .oct button { padding: .12rem .5rem; }
  .oct span { min-width: 1.8rem; text-align: center; color: var(--text); font-size: .8rem; }
  .mix { flex-wrap: wrap; }
  .mix input[type='range'] { flex: 1; min-width: 3rem; max-width: 7rem; }
  .mix input[type='number'] { width: 4.2rem; font-size: .76rem; }
  .tiny { color: var(--dim); font-size: .68rem; }
  .flags { display: flex; flex-wrap: wrap; gap: .4rem 1rem; margin-top: .6rem; padding-top: .55rem; border-top: 1px solid var(--line); }
  .flags label { color: var(--muted); font-size: .72rem; display: inline-flex; align-items: center; gap: .3rem; }
  @media (max-width: 650px) {
    .part { grid-template-columns: minmax(0, 1fr); gap: 1rem; padding: .8rem; }
  }
</style>
