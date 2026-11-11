<script lang="ts">
  import type { Snippet } from 'svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { auth, logout } from '#lib/auth.ts';
  import { NAV, allowed } from '#lib/nav.ts';
  import WsBadge from './WsBadge.svelte';

  // The existing site's header: brand, page links, a spacer, then the page's
  // own controls (a voice picker, say). The account bits sit at the far right.
  let { children, bare = false }: { children?: Snippet; bare?: boolean } = $props();

  const items = $derived(NAV.filter((n) => allowed($auth, n)));
  const current = $derived(page.url.pathname);
  const isOn = (href: string) => (href === '/' ? current === '/' : current === href || current.startsWith(href + '/'));

  async function signOut() {
    await logout();
    await goto('/login');
  }
</script>

<header class="top">
  <a class="brand" href="/">YTP<span>MACHINE</span></a>
  {#if !bare}
    <nav aria-label="Pages">
      {#each items as item (item.href)}
        <a class:on={isOn(item.href)} href={item.href} aria-current={isOn(item.href) ? 'page' : undefined}>{item.label}</a>
      {/each}
    </nav>
  {/if}
  <div class="spacer"></div>
  {@render children?.()}
  {#if !bare && $auth.user}
    <WsBadge />
    <nav aria-label="Account" class="acct">
      <a class:on={isOn('/account')} href="/account" title="Your account">{$auth.user.username}</a>
      <a href="/login" onclick={(e) => { e.preventDefault(); signOut(); }}>sign out</a>
    </nav>
  {/if}
</header>

<style>
  .acct a { white-space: nowrap; }
</style>
