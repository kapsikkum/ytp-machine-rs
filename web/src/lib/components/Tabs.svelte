<script lang="ts">
  // Pill buttons, styled as index.html's `.presets` row.
  let {
    items,
    value,
    onselect
  }: { items: { id: string; label: string }[]; value: string; onselect: (id: string) => void } = $props();
</script>

<div class="tabs" role="tablist">
  {#each items as it (it.id)}
    <button role="tab" type="button" class:on={value === it.id} aria-selected={value === it.id} onclick={() => onselect(it.id)}>{it.label}</button>
  {/each}
</div>

<style>
  .tabs { display: flex; flex-wrap: wrap; gap: .35rem; }
  button {
    cursor: pointer; background: var(--raised); border: 1px solid var(--line-2); border-radius: 999px;
    padding: .32rem .75rem; font-size: .75rem; color: var(--muted); transition: all .15s;
  }
  button:hover { color: var(--text); border-color: #4a4a52; background: var(--raised); }
  button.on { color: #000; background: var(--yellow); border-color: var(--yellow); }
</style>
