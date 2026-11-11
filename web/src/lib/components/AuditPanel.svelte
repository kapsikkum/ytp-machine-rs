<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorText } from '#lib/api.ts';
  import { formatDate } from '#lib/store.ts';
  import type { AuditEntry } from '#lib/types.ts';

  const PAGE = 50;

  let entries = $state<AuditEntry[]>([]);
  let loading = $state(true);
  let more = $state(true);
  let error = $state('');
  let filter = $state('');

  // `before` is the id of the oldest entry already shown.
  async function load(reset: boolean) {
    loading = true;
    try {
      const before = reset || entries.length === 0 ? undefined : entries[entries.length - 1].id;
      const got = await api.admin.audit({ limit: PAGE, before });
      entries = reset ? got : [...entries, ...got];
      more = got.length === PAGE;
      error = '';
    } catch (e) {
      error = errorText(e);
    } finally {
      loading = false;
    }
  }

  const shown = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter((e) => `${e.actor ?? ''} ${e.action} ${e.target ?? ''} ${e.ip ?? ''}`.toLowerCase().includes(q));
  });

  onMount(() => load(true));
</script>

<section class="panel">
  <div class="head">
    <h2>audit log</h2>
    <input type="search" bind:value={filter} placeholder="filter loaded entries…" aria-label="Filter audit log" />
    <button onclick={() => load(true)} disabled={loading}>⟲ refresh</button>
  </div>
  {#if error}<p class="err msg" role="alert">{error}</p>{/if}
  <div class="scroll">
    <table class="data">
      <thead><tr><th>when</th><th>actor</th><th>action</th><th>target</th><th>detail</th><th>ip</th></tr></thead>
      <tbody>
        {#each shown as e (e.id)}
          <tr>
            <td class="nowrap">{formatDate(e.at)}</td>
            <td>{e.actor ?? '—'}</td>
            <td><span class="tag">{e.action}</span></td>
            <td>{e.target ?? ''}</td>
            <td class="detail">{e.detail ? JSON.stringify(e.detail) : ''}</td>
            <td>{e.ip ?? ''}</td>
          </tr>
        {:else}
          <tr><td colspan="6" class="hint">{loading ? 'loading…' : 'Nothing logged yet.'}</td></tr>
        {/each}
      </tbody>
    </table>
  </div>
  {#if more && entries.length > 0}
    <p><button class="linkish" onclick={() => load(false)} disabled={loading}>{loading ? 'loading…' : 'load older entries'}</button></p>
  {/if}
</section>

<style>
  .head { display: flex; align-items: center; gap: .75rem; margin-bottom: .75rem; }
  .head h2 { flex: 1; margin: 0; }
  .head input { max-width: 16rem; font-size: .78rem; padding: .3rem .6rem; }
  .scroll { overflow-x: auto; }
  .nowrap { white-space: nowrap; }
  .detail { color: var(--dim); font-size: .7rem; max-width: 28rem; overflow-wrap: anywhere; }
  .msg { font-size: .78rem; margin-bottom: .5rem; }
  p { margin-top: .6rem; }
</style>
