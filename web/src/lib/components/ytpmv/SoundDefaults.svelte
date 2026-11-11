<script lang="ts">
  import { corpora } from '#lib/voice.ts';
  import { CHIPS } from '#lib/ytpmv/model.ts';

  let { voice = $bindable(''), chip = $bindable('') }: { voice?: string; chip?: string } = $props();
</script>

<section class="card wbcard" aria-label="Song sound defaults">
  <div class="title"><b>Song sound</b><small>Used unless a track overrides</small></div>
  <label title="The voice inherited by tracks using the song default. Individual track voices stay as chosen.">Default voice
    <select bind:value={voice} disabled={$corpora.length === 0}>
      {#if $corpora.length === 0}<option value="">no voices installed</option>{/if}
      {#each $corpora as c (c.name)}<option value={c.name}>{c.name}</option>{/each}
    </select>
  </label>
  <label title="Choose the console for the whole song. Auto tracks follow this choice; individual instrument overrides stay as chosen.">Global chip
    <select bind:value={chip} aria-label="Global chip">
      {#each CHIPS as c (c)}<option value={c}>{c || 'No emulation'}</option>{/each}
    </select>
  </label>
  <details class="help">
    <summary>About defaults</summary>
    <p>Tracks set to Auto follow the global chip. Choose an instrument in the selected track to override it. Voice menus let each track follow the default voice or use another installed voice.</p>
  </details>
</section>

<style>
  .wbcard {
    width: 100%; margin: 0; border-radius: 0; padding: .65rem 1rem; background: var(--surface);
    display: flex; flex-wrap: wrap; align-items: center; gap: .6rem 1.2rem;
  }
  .title { display: flex; flex-direction: column; gap: .2rem; margin-right: 1rem; }
  .title b { font-size: .78rem; font-weight: 500; }
  .title small { font-size: .62rem; color: var(--muted); }
  label { display: flex; flex-direction: column; align-items: flex-start; gap: .3rem; color: var(--muted); font-size: .65rem; }
  select { width: 220px; max-width: 100%; min-width: 0; min-height: 33px; font-size: .74rem; }
  .help { font-size: .65rem; color: var(--muted); max-width: 100%; margin-left: auto; }
  .help summary { cursor: pointer; padding: .5rem 0; }
  .help[open] { flex-basis: 100%; }
  .help p { margin: .3rem 0; max-width: 90ch; line-height: 1.6; }
  @media (max-width: 650px) {
    .title { flex-basis: 100%; flex-direction: row; align-items: baseline; gap: .6rem; }
    label { flex: 1 1 180px; min-width: 0; }
    select { width: 100%; }
  }
</style>
