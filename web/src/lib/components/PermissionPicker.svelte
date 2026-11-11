<script lang="ts">
  import { ALL_PERMISSIONS } from '#lib/types.ts';

  // Permissions grouped by their prefix (generate, project, corpus, …).
  let {
    selected = $bindable([]),
    available = ALL_PERMISSIONS as string[],
    disabled = false
  }: { selected?: string[]; available?: string[]; disabled?: boolean } = $props();

  const groups = $derived.by(() => {
    const m = new Map<string, string[]>();
    for (const p of available) {
      const g = p.split('.')[0];
      m.set(g, [...(m.get(g) ?? []), p]);
    }
    return [...m.entries()];
  });

  function toggle(p: string, on: boolean) {
    selected = on ? [...selected.filter((x) => x !== p), p] : selected.filter((x) => x !== p);
  }
</script>

<div class="perms">
  {#each groups as [group, perms] (group)}
    <fieldset>
      <legend>{group}</legend>
      {#each perms as p (p)}
        <label title={p}>
          <input type="checkbox" checked={selected.includes(p)} {disabled} onchange={(e) => toggle(p, e.currentTarget.checked)} />
          {p.slice(group.length + 1)}
        </label>
      {/each}
    </fieldset>
  {/each}
</div>

<style>
  .perms { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: .6rem; }
  fieldset { border: 1px solid var(--line); border-radius: 8px; padding: .4rem .6rem .5rem; background: #101012; min-width: 0; }
  legend { color: var(--yellow); font-size: .62rem; letter-spacing: 1.2px; text-transform: uppercase; padding: 0 .3rem; }
  label { display: flex; align-items: center; gap: .4rem; color: var(--muted); font-size: .74rem; padding: .12rem 0; cursor: pointer; }
</style>
