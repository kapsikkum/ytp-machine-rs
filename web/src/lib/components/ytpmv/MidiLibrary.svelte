<script lang="ts">
  // The shared MIDI library. There is no endpoint for it in the contract yet,
  // so the list is empty; the search and sort controls are the old page's.
  let search = $state('');
  let sort = $state('added');
</script>

<section class="card wbcard">
  <h2>MIDI library <span class="count">0 files</span></h2>
  <div class="lib-tools">
    <input type="search" bind:value={search} placeholder="search titles, keys, instruments…" aria-label="Search MIDI library" />
    <select bind:value={sort} title="Order by" aria-label="Sort MIDI library">
      <option value="added">newest</option>
      <option value="title">title</option>
      <option value="duration">longest</option>
      <option value="bpm">fastest</option>
      <option value="parts">most parts</option>
    </select>
  </div>
  <p class="status" role="status">Uploaded MIDI files are shared on this server. The library is not available yet.</p>
  <div class="lib-list"></div>
</section>

<style>
  .wbcard { width: 100%; padding: 1.25rem; border-radius: 0; margin: 0; }
  h2 { display: flex; gap: .8rem; align-items: baseline; font-size: .9rem; font-weight: 500; }
  .count, .status { font-size: .7rem; color: var(--muted); }
  .status { margin: .6rem 0; }
  .lib-tools { display: flex; gap: .5rem; margin: .8rem 0 .6rem; flex-wrap: wrap; }
  .lib-tools input { flex: 1; min-width: 10rem; }
  .lib-list { display: flex; flex-direction: column; gap: .5rem; max-height: 65vh; min-height: 40px; overflow-y: auto; }
</style>
