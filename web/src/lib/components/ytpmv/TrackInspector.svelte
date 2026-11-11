<script lang="ts">
  import { corpora } from '#lib/voice.ts';
  import { INSTRUMENTS, type Part } from '#lib/ytpmv/model.ts';

  let { part = $bindable(), defaultVoice, onplay }: { part: Part | undefined; defaultVoice: string; onplay: () => void } = $props();

  const synth = $derived(!!part && part.instrument !== 'auto' && part.instrument !== 'voice');
</script>

{#if part}
  <div class="part" class:instrumental={synth}>
    <div class="who">
      <div class="role">{part.role}</div>
      <div class="name">{part.name}</div>
      <div class="meta">{part.program} · {part.notes.length} notes</div>
      <button type="button" class="play" onclick={onplay}>▶ part</button>
    </div>

    {#if !synth}
      <div class="sound">
        <div class="row">
          <input type="text" bind:value={part.text} placeholder="a word or phrase" aria-label="Word for this part" />
        </div>
        <div class="row">
          <button type="button" class="play" onclick={onplay} title="Play this word">▶</button>
          <button type="button" aria-label="Previous take" disabled={part.take === 0} onclick={() => (part.take -= 1)}>‹</button>
          <span class="take">take {part.take + 1}</span>
          <button type="button" aria-label="Next take" onclick={() => (part.take += 1)}>›</button>
          <span class="badge">not scored yet</span>
        </div>
        <div class="alts"><span class="or">alternatives need the corpus word list — not available yet</span></div>
      </div>
    {/if}

    <div class="knobs">
      <div class="kv">
        <div class="kvrow">
          <span class="k">voice</span>
          <span class="v">
            <select bind:value={part.corpus} disabled={synth}>
              <option value="">song default{defaultVoice ? ` (${defaultVoice})` : ''}</option>
              {#each $corpora as c (c.name)}<option value={c.name}>{c.name}</option>{/each}
            </select>
          </span>
        </div>
        <div class="kvrow">
          <span class="k">instrument</span>
          <span class="v">
            <select bind:value={part.instrument}>
              {#each INSTRUMENTS as i (i)}<option value={i}>{i}</option>{/each}
            </select>
          </span>
        </div>
        <div class="kvrow">
          <span class="k">octave</span>
          <span class="v oct">
            <button type="button" onclick={() => (part.octave -= 1)} aria-label="Octave down">−</button>
            <span>{part.octave > 0 ? '+' : ''}{part.octave}</span>
            <button type="button" onclick={() => (part.octave += 1)} aria-label="Octave up">+</button>
          </span>
        </div>
        <div class="kvrow">
          <span class="k">mix</span>
          <span class="v mix">
            <input type="range" min="0" max="150" step="1" bind:value={part.volume} aria-label="Part volume" />
            <input type="number" min="0" max="150" bind:value={part.volume} aria-label="Volume percent" />
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
  .alts { margin-top: .5rem; }
  .or { color: var(--dim); font-size: .7rem; }
  .part select, .part input, .part button { font-size: .72rem; border-radius: 4px; min-height: 34px; }
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
