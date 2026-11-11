<script lang="ts">
  import '#lib/styles/theme.css';
  import '#lib/styles/app.css';
  import type { Snippet } from 'svelte';
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { auth, refreshAuth } from '#lib/auth.ts';
  import { NAV, allowed, itemFor } from '#lib/nav.ts';
  import { installRangeFill } from '#lib/theme.ts';
  import { connectWs, disconnectWs } from '#lib/ws.ts';
  import TopBar from '#lib/components/TopBar.svelte';

  let { children }: { children: Snippet } = $props();

  const PUBLIC = ['/login', '/setup'];

  onMount(() => {
    const undo = installRangeFill();
    refreshAuth();
    return undo;
  });

  // The route guard. Runs again whenever auth state or the path changes.
  const pathname = $derived(page.url.pathname);
  const target = $derived.by(() => {
    const a = $auth;
    if (!a.loaded || a.unreachable) return null;
    if (a.setup_required) return pathname === '/setup' ? null : '/setup' + page.url.search;
    if (!a.user) {
      if (pathname === '/login') return null;
      if (pathname === '/setup') return '/login';
      const next = pathname + page.url.search;
      return '/login' + (pathname === '/' ? '' : '?next=' + encodeURIComponent(next));
    }
    if (PUBLIC.includes(pathname)) return '/';
    // Signed in, but the page needs a permission this user lacks: fall back to
    // the first thing they can use.
    const item = itemFor(pathname);
    if (item && !allowed(a, item)) {
      const first = NAV.find((n) => allowed(a, n));
      return first ? first.href : '/account';
    }
    return null;
  });

  $effect(() => {
    if (target && target !== pathname + page.url.search) goto(target, { replaceState: true });
  });

  $effect(() => {
    if ($auth.user) connectWs();
    else disconnectWs();
  });

  const ready = $derived($auth.loaded && !$auth.unreachable && target === null);
</script>

{#if ready}
  {@render children()}
{:else if $auth.unreachable}
  <TopBar bare />
  <main class="gate">
    <section class="card">
      <div class="section-label">can't reach the server</div>
      <p class="err">{$auth.unreachable}</p>
      <p class="hint">Is <b>ytp-machine serve</b> running?</p>
      <p><button onclick={() => refreshAuth()}>try again</button></p>
    </section>
  </main>
{:else}
  <TopBar bare />
  <main class="gate"><p class="hint">loading…</p></main>
{/if}

<style>
  .gate { width: min(480px, 100% - 2rem); margin: 3rem auto; display: flex; flex-direction: column; gap: 1rem; }
  .gate .card { padding: 1.25rem; display: flex; flex-direction: column; gap: .8rem; }
  .gate p { line-height: 1.6; }
</style>
