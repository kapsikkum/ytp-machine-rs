<script lang="ts" module>
  export interface PastEntry {
    text: string;
    at: number;
  }
</script>

<script lang="ts">
  let { items, onuse, onclear }: { items: PastEntry[]; onuse: (text: string) => void; onclear: () => void } = $props();

  let find = $state('');
  let limit = $state(8);

  const matching = $derived(find.trim() ? items.filter((i) => i.text.toLowerCase().includes(find.trim().toLowerCase())) : items);

  function when(at: number): string {
    const s = Math.round((Date.now() - at) / 1000);
    if (s < 60) return 'just now';
    if (s < 3600) return `${Math.floor(s / 60)} min ago`;
    if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
    return new Date(at).toLocaleDateString();
  }
</script>

{#if items.length}
  <section class="history">
    <div class="history-head">
      <h3>recent</h3>
      {#if items.length > 5}<input type="search" bind:value={find} placeholder="find a past sentence…" aria-label="Find in history" />{/if}
      <button type="button" class="linkish" onclick={onclear}>clear</button>
    </div>
    <div class="list">
      {#each matching.slice(0, limit) as p (p.at)}
        <div class="past">
          <button type="button" class="replay" onclick={() => onuse(p.text)} title="put this back in the box">
            <span class="said">{p.text}</span>
            <span class="when">{when(p.at)}</span>
          </button>
        </div>
      {/each}
    </div>
    {#if matching.length > limit}
      <button type="button" class="linkish more" onclick={() => (limit += 10)}>{matching.length - limit} more…</button>
    {/if}
  </section>
{/if}

<style>
  h3 { font-size: .7rem; letter-spacing: 1.5px; text-transform: uppercase; color: var(--dim); font-weight: 500; margin: .5rem .25rem .5rem; }
  .history-head { display: flex; align-items: center; gap: .75rem; }
  .history-head h3 { flex: 1; }
  input { max-width: 16rem; font-size: .78rem; padding: .3rem .6rem; }
  .list { display: flex; flex-direction: column; gap: .3rem; }
  .past { border: 1px solid transparent; border-radius: 10px; transition: background .15s, border-color .15s; }
  .past:hover { background: var(--surface); border-color: var(--line); }
  .replay {
    display: flex; align-items: center; gap: .75rem; width: 100%; text-align: left;
    background: none; border: none; padding: .55rem .7rem; color: var(--muted); cursor: pointer; border-radius: 10px;
  }
  .past:hover .replay { color: var(--text); }
  .replay:hover:not(:disabled) { background: none; }
  .said { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: .82rem; }
  .when { color: var(--dim); font-size: .7rem; white-space: nowrap; }
  .more { margin: .4rem .7rem; }
</style>
