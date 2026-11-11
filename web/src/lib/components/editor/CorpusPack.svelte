<script lang="ts">
  import { api, errorText } from '#lib/api.ts';
  import { can } from '#lib/auth.ts';
  import { corpora, loadCorpora } from '#lib/voice.ts';
  import { formatBytes, formatDate } from '#lib/store.ts';

  // The "corpus pack ▾" dropdown of editor.html. Install (.tar.zst) and delete
  // are real; export has no endpoint in the contract yet.
  let file = $state<File | null>(null);
  let input = $state<HTMLInputElement>();
  let busy = $state(false);
  let msg = $state('');
  let msgKind = $state<'ok' | 'err' | ''>('');

  async function install() {
    if (!file) return;
    busy = true;
    msg = `installing ${file.name}…`;
    msgKind = '';
    try {
      const c = await api.corpora.install(file);
      msg = `installed “${c.name}”: ${c.words.toLocaleString()} words`;
      msgKind = 'ok';
      file = null;
      if (input) input.value = '';
      await loadCorpora();
    } catch (e) {
      msg = errorText(e);
      msgKind = 'err';
    } finally {
      busy = false;
    }
  }

  async function remove(name: string) {
    if (!confirm(`Delete corpus “${name}”? This removes its videos, transcripts and database.`)) return;
    try {
      await api.corpora.remove(name);
      msg = `deleted “${name}”`;
      msgKind = 'ok';
      await loadCorpora();
    } catch (e) {
      msg = errorText(e);
      msgKind = 'err';
    }
  }
</script>

<details class="pack">
  <summary>corpus pack ▾</summary>
  <div class="body">
    <button type="button" class="export" disabled title="not available yet">↓ export current corpus</button>
    <div class="hint" style="margin:.4rem 0 .7rem">
      A .tar.zst of the database, videos, transcripts and pronunciation list — move it to another machine, or keep it as a backup. <i>(export: not available yet)</i>
    </div>

    {#if $can('corpus.install')}
      <div class="row">
        <input type="file" accept=".zst,.gz,.tar,.tgz" bind:this={input} onchange={(e) => (file = e.currentTarget.files?.[0] ?? null)} />
      </div>
      <div class="row"><button type="button" class="primary" style="flex:1" disabled={busy || !file} onclick={install}>↑ install corpus pack</button></div>
    {/if}

    {#if $corpora.length}
      <div class="section-label" style="margin:.8rem 0 .4rem">installed</div>
      <div class="installed">
        {#each $corpora as c (c.name)}
          <div class="c" title="installed {formatDate(c.installed_at)}">
            <b>{c.name}</b>
            <span>{c.words.toLocaleString()} words · {formatBytes(c.size_bytes)}</span>
            {#if $can('corpus.delete')}<button type="button" class="danger" onclick={() => remove(c.name)}>delete</button>{/if}
          </div>
        {/each}
      </div>
    {/if}
    <div class="status" class:err={msgKind === 'err'} class:ok={msgKind === 'ok'}>{msg}</div>
  </div>
</details>

<style>
  .pack { position: relative; font-size: .75rem; }
  summary {
    cursor: pointer; list-style: none; padding: .36rem .75rem;
    border: 1px solid var(--line-2); border-radius: 8px; color: var(--text); background: var(--raised);
    transition: border-color .15s;
  }
  summary::-webkit-details-marker { display: none; }
  summary:hover { border-color: #4a4a52; }
  .pack[open] summary { border-color: var(--yellow); color: var(--yellow); }
  .body {
    position: absolute; top: calc(100% + .4rem); right: 0; width: min(20rem, calc(100vw - 2rem));
    background: #18181b; border: 1px solid var(--line-2); border-radius: 12px;
    box-shadow: 0 16px 40px rgba(0, 0, 0, .55); padding: .85rem; z-index: 50;
  }
  @media (max-width: 900px) { .body { right: auto; left: 0; } }
  .export { width: 100%; }
  .row { display: flex; gap: .45rem; align-items: center; margin-bottom: .55rem; }
  .installed { display: flex; flex-direction: column; gap: .3rem; max-height: 12rem; overflow-y: auto; }
  .c { display: grid; grid-template-columns: 1fr auto; gap: .1rem .5rem; align-items: center; padding: .3rem .1rem; border-bottom: 1px solid var(--line); }
  .c b { font-weight: 500; }
  .c span { grid-column: 1; color: var(--dim); font-size: .68rem; }
  .c button { grid-column: 2; grid-row: 1 / span 2; font-size: .68rem; padding: .15rem .45rem; }
  .status { min-height: 1.1em; margin-top: .5rem; color: var(--muted); }
</style>
