<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorText } from '#lib/api.ts';
  import { auth } from '#lib/auth.ts';
  import { formatDate } from '#lib/store.ts';
  import type { Token } from '#lib/types.ts';
  import PermissionPicker from './PermissionPicker.svelte';
  import SecretDialog from './SecretDialog.svelte';

  let tokens = $state<Token[]>([]);
  let loading = $state(true);
  let error = $state('');

  let name = $state('');
  let perms = $state<string[]>([]);
  let days = $state('');
  let busy = $state(false);
  let created = $state<{ name: string; secret: string } | null>(null);

  // A token can only carry permissions its owner has.
  const grantable = $derived($auth.permissions);

  async function load() {
    loading = true;
    try {
      tokens = await api.tokens.list();
      error = '';
    } catch (e) {
      error = errorText(e);
    } finally {
      loading = false;
    }
  }

  async function create(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = '';
    try {
      const d = days.trim() === '' ? undefined : Number(days);
      const r = await api.tokens.create({ name: name.trim(), permissions: perms, expires_days: d });
      created = { name: r.token.name, secret: r.secret };
      name = '';
      perms = [];
      days = '';
      await load();
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }

  async function revoke(t: Token) {
    if (!confirm(`Revoke token “${t.name}”? Anything using it stops working.`)) return;
    try {
      await api.tokens.remove(t.id);
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  onMount(load);
</script>

<div class="stack">
  <section class="panel">
    <h2>new token</h2>
    <form onsubmit={create}>
      <div class="row">
        <label>name <input type="text" bind:value={name} placeholder="e.g. render bot" required /></label>
        <label>expires after <input type="number" min="1" step="1" bind:value={days} placeholder="never" style="width:6rem" /> days</label>
      </div>
      <div class="section-label" style="margin:.6rem 0 .4rem">permissions</div>
      <PermissionPicker bind:selected={perms} available={grantable} />
      <div class="row end">
        <button class="primary" type="submit" disabled={busy || !name.trim() || perms.length === 0}>create token</button>
      </div>
    </form>
  </section>

  <section class="panel">
    <h2>your tokens</h2>
    {#if error}<p class="err msg">{error}</p>{/if}
    {#if loading}
      <p class="hint">loading…</p>
    {:else if tokens.length === 0}
      <p class="hint">No tokens yet.</p>
    {:else}
      <div class="scroll">
        <table class="data">
          <thead><tr><th>name</th><th>permissions</th><th>created</th><th>expires</th><th>last used</th><th></th></tr></thead>
          <tbody>
            {#each tokens as t (t.id)}
              <tr>
                <td>{t.name}</td>
                <td>{#each t.permissions as p (p)}<span class="tag">{p}</span>{/each}</td>
                <td>{formatDate(t.created_at)}</td>
                <td>{t.expires_at ? formatDate(t.expires_at) : 'never'}</td>
                <td>{t.last_used_at ? formatDate(t.last_used_at) : '—'}</td>
                <td><button class="danger" onclick={() => revoke(t)}>revoke</button></td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</div>

{#if created}
  <SecretDialog name={created.name} secret={created.secret} onclose={() => (created = null)} />
{/if}

<style>
  .stack { display: flex; flex-direction: column; gap: 1rem; }
  .row { display: flex; flex-wrap: wrap; gap: .6rem 1.2rem; align-items: center; }
  .row.end { justify-content: flex-end; margin-top: .8rem; }
  label { display: inline-flex; align-items: center; gap: .5rem; color: var(--muted); font-size: .78rem; }
  .scroll { overflow-x: auto; }
  .msg { font-size: .78rem; margin-bottom: .5rem; }
</style>
