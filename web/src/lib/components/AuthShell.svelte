<script lang="ts">
  import type { Snippet } from 'svelte';
  import TopBar from './TopBar.svelte';

  let { title, children }: { title: string; children: Snippet } = $props();
</script>

<TopBar bare />
<main>
  <section class="card">
    <div class="section-label">{title}</div>
    {@render children()}
  </section>
</main>

<style>
  main { width: min(420px, 100% - 2rem); margin: clamp(1.25rem, 8vh, 4rem) auto 3rem; }
  .card { padding: 1.25rem; display: flex; flex-direction: column; gap: .9rem; }
  .card :global(form) { display: flex; flex-direction: column; gap: .8rem; }
  .card :global(label.field) { display: flex; flex-direction: column; gap: .3rem; color: var(--muted); font-size: .75rem; }
  .card :global(label.field input) { font-size: .95rem; padding: .5rem .7rem; }
  .card :global(button.primary) { padding: .55rem 1rem; }
  .card :global(.msg) { font-size: .78rem; line-height: 1.5; min-height: 1.1em; }
</style>
