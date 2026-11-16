<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorText } from '#lib/api.ts';
  import type { Share } from '#lib/types.ts';

  // Owner-only. `read` can preview and export; `write` can also edit and save.
  // The server checks both the permission and the ownership, so this dialog is
  // only ever opened for a project whose access is 'owner'.
  let { projectId, onclose }: { projectId: number; onclose: () => void } = $props();

  let shares = $state<Share[]>([]);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state('');
  let username = $state('');
  let access = $state<'read' | 'write'>('read');

  async function load() {
    loading = true;
    error = '';
    try {
      shares = await api.projects.shares(projectId);
    } catch (e) {
      error = errorText(e);
    } finally {
      loading = false;
    }
  }

  async function add(e: SubmitEvent) {
    e.preventDefault();
    const name = username.trim();
    if (!name || busy) return;
    busy = true;
    error = '';
    try {
      shares = await api.projects.share(projectId, { username: name, access });
      username = '';
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  async function remove(s: Share) {
    if (busy) return;
    if (!confirm(`Stop sharing this project with ${s.username}?`)) return;
    busy = true;
    error = '';
    try {
      await api.projects.unshare(projectId, s.username);
      shares = shares.filter((x) => x.user_id !== s.user_id);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    void load();
  });
</script>

<svelte:window onkeydown={(e) => { if (e.key === 'Escape' && !busy) onclose(); }} />

<div class="scrim" role="presentation">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="share-title">
    <div class="head">
      <h3 id="share-title">share project</h3>
      <span class="hint">read can preview and export; write can also edit and save</span>
    </div>

    {#if loading}
      <p class="hint">loading shares…</p>
    {:else}
      {#if error}<p class="err" role="alert">{error}</p>{/if}

      <ul class="shares">
        {#each shares as s (s.user_id)}
          <li>
            <span class="who">{s.username}</span>
            <span class="tag">{s.access}</span>
            <span class="grow"></span>
            <button type="button" disabled={busy} onclick={() => remove(s)}>remove</button>
          </li>
        {:else}
          <li class="hint">Only you can open it so far.</li>
        {/each}
      </ul>

      <form class="add" onsubmit={add}>
        <input type="text" bind:value={username} maxlength="200" placeholder="username"
               aria-label="Username to share with" disabled={busy} />
        <select bind:value={access} disabled={busy} aria-label="Access level">
          <option value="read">read</option>
          <option value="write">write</option>
        </select>
        <button type="submit" class="primary" disabled={busy || !username.trim()}>{busy ? 'saving…' : 'share'}</button>
      </form>
    {/if}

    <div class="actions">
      <button type="button" onclick={onclose} disabled={busy}>close</button>
    </div>
  </div>
</div>

<style>
  .scrim { position: fixed; inset: 0; background: rgba(0, 0, 0, .7); z-index: 100; display: flex; align-items: center; justify-content: center; }
  .dialog {
    background: #18181b; border: 1px solid var(--line-2); border-radius: 12px; padding: 1.25rem;
    width: min(92vw, 520px); max-height: 90vh; overflow: auto;
    box-shadow: 0 20px 40px rgba(0, 0, 0, .6); display: flex; flex-direction: column; gap: .9rem;
  }
  .head { display: flex; align-items: baseline; justify-content: space-between; gap: .75rem; flex-wrap: wrap; }
  h3 { font-size: 1rem; font-weight: 600; color: var(--text); }
  .shares { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: .3rem; }
  .shares li { display: flex; align-items: center; gap: .45rem; padding: .4rem .55rem; border: 1px solid var(--line); border-radius: 8px; font-size: .8rem; }
  .shares li.hint { border-style: dashed; color: var(--dim); }
  .shares .who { color: var(--text); }
  .shares .grow { flex: 1; }
  .shares button { font-size: .7rem; padding: .18rem .5rem; }
  .add { display: flex; align-items: center; gap: .4rem; flex-wrap: wrap; border-top: 1px dashed var(--line); padding-top: .75rem; }
  .add input { flex: 1; min-width: 9rem; }
  .add select { font-size: .78rem; }
  .actions { display: flex; justify-content: flex-end; gap: .5rem; flex-wrap: wrap; }
  .err { color: var(--red); font-size: .76rem; margin: 0; }
  .hint { color: var(--dim); font-size: .72rem; margin: 0; }
</style>
