<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { setup } from '#lib/auth.ts';
  import { errorText } from '#lib/api.ts';
  import AuthShell from '#lib/components/AuthShell.svelte';

  // The server prints /setup?token=… on first run; the token rides along here.
  let token = $state(page.url.searchParams.get('token') ?? '');
  let username = $state('admin');
  let password = $state('');
  let confirm = $state('');
  let busy = $state(false);
  let error = $state('');

  const mismatch = $derived(confirm !== '' && confirm !== password);
  const tooShort = $derived(password !== '' && password.length < 8);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (mismatch || tooShort) return;
    busy = true;
    error = '';
    try {
      await setup(token.trim(), username.trim(), password);
      await goto('/', { replaceState: true });
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>first-run setup · YTP Machine</title></svelte:head>

<AuthShell title="first-run setup">
  <p class="hint">No users exist yet. Create the first <b>admin</b> account. The setup token is in the link the server printed when it started.</p>
  <form onsubmit={submit}>
    <label class="field">setup token
      <input type="text" bind:value={token} autocomplete="off" spellcheck="false" required />
    </label>
    <label class="field">admin username
      <input type="text" bind:value={username} autocomplete="username" autocapitalize="off" spellcheck="false" required />
    </label>
    <label class="field">password
      <input type="password" bind:value={password} autocomplete="new-password" minlength="8" required />
    </label>
    <label class="field">repeat password
      <input type="password" bind:value={confirm} autocomplete="new-password" required />
    </label>
    <div class="msg err" role="alert">
      {#if error}{error}{:else if tooShort}use at least 8 characters{:else if mismatch}the passwords don't match{/if}
    </div>
    <button class="primary" type="submit" disabled={busy || !token || !username || !password || mismatch || tooShort}>
      {busy ? 'creating…' : 'create admin'}
    </button>
  </form>
</AuthShell>
