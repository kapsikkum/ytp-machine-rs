<script lang="ts">
  import type { MidiEntry } from '#lib/types.ts';
  import { clock } from '#lib/timeline.ts';
  import { formatBytes } from '#lib/store.ts';

  let {
    entries,
    loading = false,
    error = '',
    opening = null,
    onopen,
    onrename,
    ondelete
  }: {
    entries: MidiEntry[];
    loading?: boolean;
    error?: string;
    opening?: string | null;
    onopen: (id: string) => void;
    onrename: (id: string, title: string) => void | Promise<void>;
    ondelete: (id: string) => void | Promise<void>;
  } = $props();

  let search = $state('');
  let sort = $state('added');
  let editing = $state<string | null>(null);
  let draft = $state('');

  const shown = $derived.by(() => {
    const q = search.trim().toLowerCase();
    const list = entries.filter((e) => !q || `${e.title} ${e.key} ${e.description}`.toLowerCase().includes(q));
    const by: Record<string, (a: MidiEntry, b: MidiEntry) => number> = {
      added: (a, b) => b.added - a.added,
      title: (a, b) => a.title.localeCompare(b.title),
      duration: (a, b) => b.duration - a.duration,
      bpm: (a, b) => b.bpm - a.bpm,
      parts: (a, b) => b.parts - a.parts
    };
    return list.toSorted(by[sort] ?? by.added);
  });

  function edit(e: MidiEntry) {
    editing = e.midi_id;
    draft = e.title;
  }

  async function save(id: string) {
    const t = draft.trim();
    editing = null;
    if (t) await onrename(id, t);
  }
</script>

<section class="card wbcard">
  <h2>Your MIDI files <span class="count">{entries.length} file{entries.length === 1 ? '' : 's'}</span></h2>
  <div class="lib-tools">
    <input type="search" bind:value={search} placeholder="search titles, keys, instruments…" aria-label="Search MIDI library" />
    <select bind:value={sort} title="Order by" aria-label="Sort MIDI library">
      <option value="added">newest</option>
      <option value="title">title</option>
      <option value="duration">longest</option>
      <option value="bpm">fastest</option>
      <option value="parts">most parts</option>
    </select>
  </div>
  {#if error}
    <p class="status err" role="alert">{error}</p>
  {:else if loading}
    <p class="status" role="status">loading…</p>
  {:else if entries.length === 0}
    <p class="status" role="status">Nothing uploaded yet. Files you upload are kept here, for you only.</p>
  {/if}
  <div class="lib-list">
    {#each shown as e (e.midi_id)}
      <div class="item" class:opening={opening === e.midi_id}>
        <div class="main">
          {#if editing === e.midi_id}
            <input
              class="rename"
              bind:value={draft}
              maxlength="80"
              aria-label="MIDI title"
              onkeydown={(ev) => { if (ev.key === 'Enter') void save(e.midi_id); if (ev.key === 'Escape') editing = null; }}
            />
          {:else}
            <button type="button" class="title" onclick={() => onopen(e.midi_id)}>{e.title || 'Untitled MIDI'}</button>
          {/if}
          <div class="meta">
            {Math.round(e.bpm)} bpm · {e.key} · {e.time_signature} · {clock(e.duration).replace(/\.\d+$/, '')} · {e.parts} part{e.parts === 1 ? '' : 's'}{e.drums ? ` + ${e.drums} drum` : ''} · {e.notes.toLocaleString()} notes · {formatBytes(e.bytes)}
          </div>
        </div>
        <div class="tools">
          {#if editing === e.midi_id}
            <button type="button" onclick={() => save(e.midi_id)}>save</button>
            <button type="button" onclick={() => (editing = null)}>cancel</button>
          {:else}
            <button type="button" class="primary" disabled={opening !== null} onclick={() => onopen(e.midi_id)}>{opening === e.midi_id ? 'opening…' : 'open'}</button>
            <button type="button" onclick={() => edit(e)}>rename</button>
            <button type="button" class="danger" onclick={() => ondelete(e.midi_id)}>delete</button>
          {/if}
        </div>
      </div>
    {/each}
  </div>
</section>

<style>
  .wbcard { width: 100%; padding: 1.25rem; border-radius: 0; margin: 0; }
  h2 { display: flex; gap: .8rem; align-items: baseline; font-size: .9rem; font-weight: 500; }
  .count, .status { font-size: .7rem; color: var(--muted); }
  .status { margin: .6rem 0; }
  .status.err { color: var(--red, #ff5a5a); }
  .lib-tools { display: flex; gap: .5rem; margin: .8rem 0 .6rem; flex-wrap: wrap; }
  .lib-tools input { flex: 1; min-width: 10rem; }
  .lib-list { display: flex; flex-direction: column; gap: .5rem; max-height: 65vh; min-height: 40px; overflow-y: auto; }
  .item { display: flex; align-items: center; gap: .8rem; flex-wrap: wrap; padding: .6rem .8rem; border: 1px solid var(--line); background: var(--surface); }
  .item.opening { border-color: var(--yellow); }
  .main { flex: 1; min-width: 14rem; }
  .title { border: 0; background: transparent; padding: 0; font-size: .86rem; color: var(--text); text-align: left; }
  .title:hover { color: var(--yellow); background: transparent; }
  .meta { color: var(--muted); font-size: .68rem; margin-top: .2rem; }
  .rename { width: 100%; font-size: .85rem; }
  .tools { display: flex; gap: .4rem; }
  .tools button { font-size: .72rem; min-height: 32px; }
</style>
