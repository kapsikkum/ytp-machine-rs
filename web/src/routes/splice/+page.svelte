<script lang="ts">
  import { NOT_AVAILABLE } from '#lib/api.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import VoiceSelect from '#lib/components/VoiceSelect.svelte';

  // The splice editor: how a word the voice never said gets built from pieces
  // of words it did. The lookup / recipe endpoints are not in the contract yet,
  // so this is the old layout, with its controls answering "not available yet".
  let word = $state('');
  let mode = $state('strict');
  let gain = $state(2);
  let spectrogram = $state(false);
  let status = $state('');
  let showDiag = $state(false);

  const unavailable = () => (status = NOT_AVAILABLE);
  function look(e: SubmitEvent) {
    e.preventDefault();
    if (word.trim()) unavailable();
  }
</script>

<svelte:head><title>splice editor · YTP Machine</title></svelte:head>

<TopBar>
  <VoiceSelect title="Which voice to edit." />
  <button type="button" class="ghost diag-btn" onclick={() => (showDiag = !showDiag)} title="Check corpus health and view diagnostic metrics">health &amp; diagnostics</button>
</TopBar>

<div class="page">
  {#if showDiag}
    <div class="panel diag">
      <div class="diag-head">
        <h2>Corpus Health &amp; Diagnostics</h2>
        <button type="button" class="ghost sm" onclick={() => (showDiag = false)}>✕ Close</button>
      </div>
      <p class="hint">Not available yet.</p>
    </div>
  {/if}

  <div class="layout">
    <div>
      <div class="panel">
        <form class="row" onsubmit={look}>
          <input bind:value={word} placeholder="a word to build" style="flex:1" class="word" />
          <select bind:value={mode} title="strict uses only known pronunciations; loose allows near-enough phonemes; desperate will skip a sound it cannot find">
            <option value="strict">strict</option>
            <option value="loose">loose</option>
            <option value="desperate">desperate</option>
          </select>
          <button class="primary" type="submit">look up</button>
        </form>
        <div class="meta">Type a word. You get the sounds it needs, what the splicer currently does with it, and every word in the corpus that could supply each piece.</div>
        <div class="phones"></div>
        <div class="hint">
          Click the gaps between sounds to move the cuts. Each piece is then taken from one word, whole or trimmed. Every candidate clip can be heard before you choose it,
          and each piece shows its clip's waveform: drag the yellow edges to move exactly where it is cut, click anywhere on it to hear from there, space to hear the cut again.
        </div>
        <div class="status" class:warn={status === NOT_AVAILABLE} role="status">{status}</div>
      </div>

      <div class="panel">
        <h2>Pieces</h2>
        <div class="row meta" style="align-items:center;gap:.75rem">
          <span>waveform loudness</span>
          <input type="range" min="1" max="40" step="0.5" bind:value={gain} style="flex:1;max-width:14rem" />
          <label class="check"><input type="checkbox" bind:checked={spectrogram} /> spectrogram</label>
        </div>
        <div class="hint">Look a word up to see its pieces.</div>
        <div class="row" style="margin-top:.8rem">
          <button type="button" class="primary" onclick={unavailable}>▶ hear it</button>
          <button type="button" onclick={unavailable}>save as the recipe for this word</button>
          <button type="button" class="danger" onclick={unavailable}>forget the recipe</button>
        </div>
        <div class="hint">
          Saving writes the pieces down against this corpus, and every future generation of the word uses them instead of searching.
          The search is good at most words and blind to the ones it gets wrong, which is what this is for.
        </div>
      </div>
    </div>

    <div>
      <div class="panel">
        <h2>What the splicer does now</h2>
        <div class="meta">—</div>
        <div class="row" style="margin-top:.5rem"><button disabled>load it into the pieces</button></div>
      </div>
      <div class="panel">
        <h2>Saved recipes</h2>
        <div class="meta">—</div>
      </div>
    </div>
  </div>
</div>

<style>
  .page { width: min(1240px, 100% - 2rem); margin: 1.25rem auto 3rem; font-size: 13px; }
  .layout { display: grid; grid-template-columns: minmax(0, 1fr) 22rem; gap: 1rem; align-items: start; }
  @media (max-width: 900px) { .layout { grid-template-columns: minmax(0, 1fr); } }
  .panel { margin-bottom: 1rem; }
  .diag-btn { padding: .28rem .65rem; font-size: .75rem; border-radius: 7px; color: var(--text); margin-left: .75rem; }
  .diag-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: .75rem; }
  .diag-head h2 { margin: 0; font-size: .75rem; letter-spacing: 1px; color: var(--yellow); font-weight: 600; }
  .sm { padding: .2rem .6rem; font-size: .72rem; }
  .row { display: flex; gap: .45rem; align-items: center; margin-bottom: .55rem; flex-wrap: wrap; }
  .word { font-size: 1.05rem; padding: .5rem .75rem; }
  .meta { color: var(--muted); line-height: 1.7; }
  .hint { margin-top: .5rem; }
  .check { display: flex; align-items: center; gap: .35rem; cursor: pointer; font-size: .75rem; color: var(--text); }
  .phones { display: flex; align-items: stretch; flex-wrap: wrap; margin: .5rem 0 .9rem; }
  .status { margin-top: .6rem; min-height: 1.2em; color: var(--muted); }
  .status.warn { color: var(--amber); }
</style>
