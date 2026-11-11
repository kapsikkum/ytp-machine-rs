<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorText } from '#lib/api.ts';
  import type { Quota, Role } from '#lib/types.ts';
  import PermissionPicker from './PermissionPicker.svelte';

  const DEFAULT_QUOTA: Quota = { max_concurrent_jobs: 1, max_render_seconds: 60, max_height: 720, requests_per_minute: 30 };
  const QUOTA_FIELDS: { key: keyof Quota; label: string }[] = [
    { key: 'max_concurrent_jobs', label: 'concurrent jobs' },
    { key: 'max_render_seconds', label: 'render seconds' },
    { key: 'max_height', label: 'max height (px)' },
    { key: 'requests_per_minute', label: 'requests / min' }
  ];

  let roles = $state<Role[]>([]);
  let loading = $state(true);
  let error = $state('');
  let open = $state<string | null>(null);

  // Editing copy of the open role.
  let draftPerms = $state<string[]>([]);
  let draftQuota = $state<Quota>({ ...DEFAULT_QUOTA });

  let newName = $state('');
  let newPerms = $state<string[]>([]);
  let newQuota = $state<Quota>({ ...DEFAULT_QUOTA });
  let busy = $state(false);

  async function load() {
    loading = true;
    try {
      roles = await api.admin.roles.list();
      error = '';
    } catch (e) {
      error = errorText(e);
    } finally {
      loading = false;
    }
  }

  function toggle(r: Role) {
    if (open === r.name) {
      open = null;
      return;
    }
    open = r.name;
    draftPerms = [...r.permissions];
    draftQuota = { ...DEFAULT_QUOTA, ...r.quota };
  }

  async function run(fn: () => Promise<unknown>) {
    error = '';
    try {
      await fn();
      await load();
    } catch (e) {
      error = errorText(e);
    }
  }

  const save = (r: Role) =>
    run(async () => {
      await api.admin.roles.update(r.name, { permissions: draftPerms, quota: draftQuota });
      open = null;
    });
  const remove = (r: Role) => {
    if (!confirm(`Delete role “${r.name}”?`)) return;
    return run(() => api.admin.roles.remove(r.name));
  };
  async function create(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    await run(async () => {
      await api.admin.roles.create({ name: newName.trim(), permissions: newPerms, quota: newQuota });
      newName = '';
      newPerms = [];
      newQuota = { ...DEFAULT_QUOTA };
    });
    busy = false;
  }

  onMount(load);
</script>

<div class="stack">
  {#if error}<p class="err msg" role="alert">{error}</p>{/if}

  <section class="panel">
    <h2>roles</h2>
    {#if loading}
      <p class="hint">loading…</p>
    {:else}
      <div class="list">
        {#each roles as r (r.name)}
          <div class="role" class:open={open === r.name}>
            <button class="head" onclick={() => toggle(r)} aria-expanded={open === r.name}>
              <b>{r.name}</b>
              {#if r.builtin}<span class="tag">built-in</span>{/if}
              <span class="count">{r.permissions.length} permissions</span>
            </button>
            {#if open === r.name}
              <div class="body">
                <PermissionPicker bind:selected={draftPerms} />
                <div class="section-label" style="margin:1rem 0 .4rem">quota</div>
                <div class="quota">
                  {#each QUOTA_FIELDS as f (f.key)}
                    <label>{f.label} <input type="number" min="0" step="1" bind:value={draftQuota[f.key]} /></label>
                  {/each}
                </div>
                <div class="actions">
                  <button class="danger" disabled={r.builtin} title={r.builtin ? 'built-in roles cannot be deleted' : ''} onclick={() => remove(r)}>delete role</button>
                  <span class="grow"></span>
                  <button onclick={() => (open = null)}>cancel</button>
                  <button class="primary" onclick={() => save(r)}>save</button>
                </div>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </section>

  <section class="panel">
    <h2>new role</h2>
    <form onsubmit={create}>
      <label class="name">name <input type="text" bind:value={newName} placeholder="e.g. moderator" required /></label>
      <PermissionPicker bind:selected={newPerms} />
      <div class="section-label" style="margin:1rem 0 .4rem">quota</div>
      <div class="quota">
        {#each QUOTA_FIELDS as f (f.key)}
          <label>{f.label} <input type="number" min="0" step="1" bind:value={newQuota[f.key]} /></label>
        {/each}
      </div>
      <div class="actions"><span class="grow"></span><button class="primary" type="submit" disabled={busy || !newName.trim()}>create role</button></div>
    </form>
  </section>
</div>

<style>
  .stack { display: flex; flex-direction: column; gap: 1rem; }
  .list { display: flex; flex-direction: column; gap: .4rem; }
  .role { border: 1px solid var(--line); border-radius: 10px; background: #101012; }
  .role.open { border-color: var(--line-2); }
  .head { width: 100%; justify-content: flex-start; gap: .6rem; background: transparent; border: none; padding: .65rem .8rem; font-size: .85rem; }
  .head:hover:not(:disabled) { background: transparent; }
  .head b { font-weight: 500; }
  .head .tag { margin: 0; }
  .count { color: var(--dim); font-size: .72rem; margin-left: auto; }
  .body { padding: .2rem .8rem .9rem; }
  .quota { display: flex; flex-wrap: wrap; gap: .6rem 1.2rem; }
  label { display: inline-flex; align-items: center; gap: .5rem; color: var(--muted); font-size: .78rem; }
  .quota input { width: 6rem; }
  .name { margin-bottom: .8rem; }
  .actions { display: flex; gap: .5rem; align-items: center; margin-top: 1rem; }
  .grow { flex: 1; }
  .msg { font-size: .78rem; }
</style>
