<script lang="ts">
  import { onMount } from 'svelte';
  import { corpora, loadCorpora, selectedVoice, watchCorpora } from '#lib/voice.ts';

  // The header's "voice" picker, as on the existing pages: only shown when
  // there is a choice to make.
  let { label = 'voice', title = '' }: { label?: string; title?: string } = $props();

  onMount(() => {
    watchCorpora();
    void loadCorpora();
  });
</script>

{#if $corpora.length > 1}
  <label class="pill-select" {title}>{label}
    <select bind:value={$selectedVoice}>
      {#each $corpora as c (c.name)}<option value={c.name}>{c.name}</option>{/each}
    </select>
  </label>
{/if}
