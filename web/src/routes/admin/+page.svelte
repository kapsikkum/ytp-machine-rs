<script lang="ts">
  import { can } from '#lib/auth.ts';
  import TopBar from '#lib/components/TopBar.svelte';
  import Tabs from '#lib/components/Tabs.svelte';
  import UsersPanel from '#lib/components/UsersPanel.svelte';
  import RolesPanel from '#lib/components/RolesPanel.svelte';
  import TokensPanel from '#lib/components/TokensPanel.svelte';
  import AuditPanel from '#lib/components/AuditPanel.svelte';

  const tabs = $derived(
    [
      { id: 'users', label: 'users', perm: 'users.manage' },
      { id: 'roles', label: 'roles', perm: 'roles.manage' },
      { id: 'tokens', label: 'tokens', perm: 'tokens.create' },
      { id: 'audit', label: 'audit log', perm: 'audit.view' }
    ].filter((t) => $can(t.perm))
  );

  let chosen = $state('');
  const tab = $derived(tabs.some((t) => t.id === chosen) ? chosen : (tabs[0]?.id ?? ''));
</script>

<svelte:head><title>admin · YTP Machine</title></svelte:head>

<TopBar />
<div class="subbar">
  <span class="title">admin</span>
  <Tabs items={tabs} value={tab} onselect={(id) => (chosen = id)} />
</div>

<div class="page">
  {#if tab === 'users'}
    <UsersPanel />
  {:else if tab === 'roles'}
    <RolesPanel />
  {:else if tab === 'tokens'}
    <TokensPanel />
  {:else if tab === 'audit'}
    <AuditPanel />
  {:else}
    <p class="hint">Nothing to administer with your permissions.</p>
  {/if}
</div>

<style>
  .page { width: min(1240px, 100% - 2rem); margin: 1.25rem auto 3rem; }
</style>
