<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorText } from '#lib/api.ts';
  import { auth, can } from '#lib/auth.ts';
  import { formatDate } from '#lib/store.ts';
  import type { User } from '#lib/types.ts';

  let users = $state<User[]>([]);
  let roleNames = $state<string[]>(['admin', 'editor', 'creator', 'guest']);
  let loading = $state(true);
  let error = $state('');

  let username = $state('');
  let password = $state('');
  let newRoles = $state<string[]>(['creator']);
  let busy = $state(false);

  let editing = $state<number | null>(null);
  let editRoles = $state<string[]>([]);
  let resetFor = $state<number | null>(null);
  let resetPw = $state('');

  async function load() {
    loading = true;
    try {
      users = await api.admin.users.list();
      error = '';
      // Role names come from /admin/roles when allowed; otherwise the built-ins.
      if ($can('roles.manage')) {
        try {
          roleNames = (await api.admin.roles.list()).map((r) => r.name);
        } catch {
          /* keep the built-ins */
        }
      }
    } catch (e) {
      error = errorText(e);
    } finally {
      loading = false;
    }
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

  async function create(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    await run(async () => {
      await api.admin.users.create({ username: username.trim(), password, roles: newRoles });
      username = '';
      password = '';
      newRoles = ['creator'];
    });
    busy = false;
  }

  function startEdit(u: User) {
    editing = u.id;
    editRoles = [...u.roles];
  }
  const saveRoles = (u: User) =>
    run(async () => {
      await api.admin.users.update(u.id, { roles: editRoles });
      editing = null;
    });
  const toggleDisabled = (u: User) => run(() => api.admin.users.update(u.id, { disabled: !u.disabled }));
  const resetPassword = (u: User) =>
    run(async () => {
      await api.admin.users.update(u.id, { password: resetPw });
      resetFor = null;
      resetPw = '';
    });
  const remove = (u: User) => {
    if (!confirm(`Delete user “${u.username}”? Their tokens stop working.`)) return;
    return run(() => api.admin.users.remove(u.id));
  };

  function flip(list: string[], r: string, on: boolean): string[] {
    return on ? [...list.filter((x) => x !== r), r] : list.filter((x) => x !== r);
  }

  onMount(load);
</script>

<div class="stack">
  <section class="panel">
    <h2>new user</h2>
    <form onsubmit={create}>
      <div class="row">
        <label>username <input type="text" bind:value={username} autocomplete="off" autocapitalize="off" required /></label>
        <label>password <input type="password" bind:value={password} autocomplete="new-password" minlength="8" required /></label>
      </div>
      <div class="row">
        <span class="k">roles</span>
        {#each roleNames as r (r)}
          <label><input type="checkbox" checked={newRoles.includes(r)} onchange={(e) => (newRoles = flip(newRoles, r, e.currentTarget.checked))} /> {r}</label>
        {/each}
        <span class="grow"></span>
        <button class="primary" type="submit" disabled={busy || !username.trim() || password.length < 8}>add user</button>
      </div>
    </form>
  </section>

  <section class="panel">
    <h2>users</h2>
    {#if error}<p class="err msg" role="alert">{error}</p>{/if}
    {#if loading}
      <p class="hint">loading…</p>
    {:else}
      <div class="scroll">
        <table class="data">
          <thead><tr><th>user</th><th>roles</th><th>created</th><th>status</th><th></th></tr></thead>
          <tbody>
            {#each users as u (u.id)}
              <tr>
                <td>{u.username}{#if u.id === $auth.user?.id} <span class="tag">you</span>{/if}</td>
                <td>
                  {#if editing === u.id}
                    <span class="edit">
                      {#each roleNames as r (r)}
                        <label><input type="checkbox" checked={editRoles.includes(r)} onchange={(e) => (editRoles = flip(editRoles, r, e.currentTarget.checked))} /> {r}</label>
                      {/each}
                      <button class="primary" onclick={() => saveRoles(u)}>save</button>
                      <button onclick={() => (editing = null)}>cancel</button>
                    </span>
                  {:else}
                    {#each u.roles as r (r)}<span class="tag">{r}</span>{/each}
                  {/if}
                </td>
                <td>{formatDate(u.created_at)}</td>
                <td>{#if u.disabled}<span class="tag bad">disabled</span>{:else}<span class="tag good">active</span>{/if}</td>
                <td class="acts">
                  {#if resetFor === u.id}
                    <input type="password" bind:value={resetPw} placeholder="new password" autocomplete="new-password" style="width:10rem" />
                    <button class="primary" disabled={resetPw.length < 8} onclick={() => resetPassword(u)}>set</button>
                    <button onclick={() => (resetFor = null)}>cancel</button>
                  {:else}
                    <button onclick={() => startEdit(u)}>roles</button>
                    <button onclick={() => { resetFor = u.id; resetPw = ''; }}>password</button>
                    <button onclick={() => toggleDisabled(u)} disabled={u.id === $auth.user?.id}>{u.disabled ? 'enable' : 'disable'}</button>
                    <button class="danger" onclick={() => remove(u)} disabled={u.id === $auth.user?.id}>delete</button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</div>

<style>
  .stack { display: flex; flex-direction: column; gap: 1rem; }
  .row { display: flex; flex-wrap: wrap; gap: .6rem 1.2rem; align-items: center; margin-bottom: .6rem; }
  .row:last-child { margin-bottom: 0; }
  .k { color: var(--dim); font-size: .7rem; letter-spacing: 1.2px; text-transform: uppercase; }
  .grow { flex: 1; }
  label { display: inline-flex; align-items: center; gap: .45rem; color: var(--muted); font-size: .78rem; }
  .edit { display: inline-flex; flex-wrap: wrap; align-items: center; gap: .4rem .8rem; }
  .acts { white-space: nowrap; }
  .acts :global(button) { font-size: .72rem; padding: .22rem .5rem; margin-right: .2rem; }
  .scroll { overflow-x: auto; }
  .msg { font-size: .78rem; margin-bottom: .5rem; }
</style>
