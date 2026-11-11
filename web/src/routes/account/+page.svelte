<script lang="ts">
  import { auth, can } from '#lib/auth.ts';
  import { api, errorText } from '#lib/api.ts';
  import { formatDate } from '#lib/store.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import TokensPanel from '#lib/components/TokensPanel.svelte';

  let current = $state('');
  let next = $state('');
  let confirmPw = $state('');
  let busy = $state(false);
  let error = $state('');
  let done = $state(false);

  const mismatch = $derived(confirmPw !== '' && confirmPw !== next);
  const tooShort = $derived(next !== '' && next.length < 8);

  async function change(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = '';
    done = false;
    try {
      await api.auth.password(current, next);
      current = next = confirmPw = '';
      done = true;
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>account · YTP Machine</title></svelte:head>

<TopBar />
<div class="subbar"><span class="title">account</span></div>

<div class="page">
  <section class="panel">
    <h2>you</h2>
    {#if $auth.user}
      <p class="who"><b>{$auth.user.username}</b> · since {formatDate($auth.user.created_at)}</p>
      <p>{#each $auth.user.roles as r (r)}<span class="tag">{r}</span>{/each}</p>
      <details class="hint">
        <summary>{$auth.permissions.length} permissions</summary>
        <p>{#each $auth.permissions as p (p)}<span class="tag">{p}</span>{/each}</p>
      </details>
    {/if}
  </section>

  <section class="panel">
    <h2>change password</h2>
    <form onsubmit={change}>
      <label>current <input type="password" bind:value={current} autocomplete="current-password" required /></label>
      <label>new <input type="password" bind:value={next} autocomplete="new-password" minlength="8" required /></label>
      <label>repeat <input type="password" bind:value={confirmPw} autocomplete="new-password" required /></label>
      <div class="msg" role="status">
        {#if error}<span class="err">{error}</span>
        {:else if done}<span class="ok">password changed</span>
        {:else if tooShort}<span class="err">use at least 8 characters</span>
        {:else if mismatch}<span class="err">the passwords don't match</span>{/if}
      </div>
      <div><button class="primary" type="submit" disabled={busy || !current || !next || mismatch || tooShort}>change password</button></div>
    </form>
  </section>

  {#if $can('tokens.create')}
    <TokensPanel />
  {/if}
</div>

<style>
  .page { width: min(1000px, 100% - 2rem); margin: 1.25rem auto 3rem; display: flex; flex-direction: column; gap: 1rem; }
  .who { margin-bottom: .5rem; color: var(--muted); }
  .who b { color: var(--text); font-weight: 500; }
  form { display: flex; flex-direction: column; gap: .7rem; max-width: 22rem; }
  label { display: flex; flex-direction: column; gap: .3rem; color: var(--muted); font-size: .75rem; }
  .msg { font-size: .78rem; min-height: 1.1em; }
  details p { margin-top: .5rem; }
</style>
