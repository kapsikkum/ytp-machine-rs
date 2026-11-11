<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { login } from '#lib/auth.ts';
  import { ApiError, errorText } from '#lib/api.ts';
  import AuthShell from '#lib/components/AuthShell.svelte';

  let username = $state('');
  let password = $state('');
  let busy = $state(false);
  let error = $state('');

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = '';
    try {
      await login(username.trim(), password);
      const next = page.url.searchParams.get('next');
      await goto(next && next.startsWith('/') && !next.startsWith('//') ? next : '/', { replaceState: true });
    } catch (err) {
      error = err instanceof ApiError && err.status === 429 ? 'too many attempts — wait a moment and try again' : errorText(err);
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>sign in · YTP Machine</title></svelte:head>

<AuthShell title="sign in">
  <form onsubmit={submit}>
    <label class="field">username
      <input type="text" bind:value={username} autocomplete="username" autocapitalize="off" spellcheck="false" required />
    </label>
    <label class="field">password
      <input type="password" bind:value={password} autocomplete="current-password" required />
    </label>
    <div class="msg err" role="alert">{error}</div>
    <button class="primary" type="submit" disabled={busy || !username || !password}>{busy ? 'signing in…' : 'sign in'}</button>
  </form>
</AuthShell>
