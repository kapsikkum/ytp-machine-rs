<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorText } from '#lib/api.ts';
  import { formatDate } from '#lib/store.ts';
  import { clock } from '#lib/timeline.ts';
  import type { MediaAsset } from '#lib/types.ts';
  import TopBar from '#lib/components/TopBar.svelte';

  // The user's private assets. The server keeps them under media/<user>/ and
  // refuses another user's file, so this page only ever lists the caller's own.
  const ACCEPT = '.mp4,.mov,.webm,.mkv,.wav,.mp3,.m4a,.aac,.flac,.ogg,.png,.jpg,.jpeg,.gif,.webp';

  let assets = $state<MediaAsset[]>([]);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state('');
  let note = $state('');
  let file = $state<File | null>(null);
  let name = $state('');
  let input = $state<HTMLInputElement>();

  async function load() {
    loading = true;
    error = '';
    try {
      assets = await api.media.list();
    } catch (e) {
      error = errorText(e);
    } finally {
      loading = false;
    }
  }

  async function upload(e: SubmitEvent) {
    e.preventDefault();
    if (!file || busy) return;
    busy = true;
    error = '';
    note = '';
    try {
      const a = await api.media.upload(file, name);
      note = `uploaded ${a.name}`;
      file = null;
      name = '';
      if (input) input.value = '';
      await load();
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }

  async function remove(a: MediaAsset) {
    const used = a.referenced_by
      ? `\n\n${a.referenced_by} project${a.referenced_by === 1 ? '' : 's'} still name it; those renders will fail until the clip is changed.`
      : '';
    if (!confirm(`Delete ${a.name}?${used}`)) return;
    busy = true;
    error = '';
    note = '';
    try {
      await api.media.remove(a.id);
      assets = assets.filter((x) => x.id !== a.id);
      note = `deleted ${a.name}`;
    } catch (err) {
      error = errorText(err);
    } finally {
      busy = false;
    }
  }

  const size = (n: number) => (n >= 1 << 20 ? `${(n / (1 << 20)).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);
  const shape = (a: MediaAsset) =>
    a.width && a.height ? `${a.width} × ${a.height}` : a.duration ? clock(a.duration) : '—';

  onMount(() => {
    void load();
  });
</script>

<svelte:head><title>media · YTP Machine</title></svelte:head>

<TopBar />
<div class="subbar">
  <span class="title">media</span>
  <span class="meta">{assets.length} asset{assets.length === 1 ? '' : 's'}</span>
  <span class="grow"></span>
  {#if note}<span class="note">{note}</span>{/if}
</div>

<div class="page">
  {#if error}<p class="err" role="alert">{error}</p>{/if}

  <section class="panel">
    <h2>upload</h2>
    <form class="upload" onsubmit={upload}>
      <input type="file" accept={ACCEPT} bind:this={input} disabled={busy}
             onchange={(e) => (file = e.currentTarget.files?.[0] ?? null)} aria-label="File to upload" />
      <input type="text" bind:value={name} maxlength="200" disabled={busy}
             placeholder="name (optional)" aria-label="Name (optional)" />
      <button type="submit" class="primary" disabled={busy || !file}>{busy ? 'uploading…' : 'upload'}</button>
    </form>
    <p class="hint">video mp4/mov/webm/mkv · audio wav/mp3/m4a/aac/flac/ogg · image png/jpg/jpeg/gif/webp · at most 200 MB. Files are private to you.</p>
  </section>

  <section class="panel">
    <h2>your assets</h2>
    <div class="scroll">
      <table class="data">
        <thead>
          <tr><th>name</th><th>kind</th><th>size</th><th>media</th><th>uploaded</th><th>used by</th><th></th></tr>
        </thead>
        <tbody>
          {#each assets as a (a.id)}
            <tr>
              <td class="name">{a.name}</td>
              <td><span class="tag">{a.kind}</span></td>
              <td class="nowrap">{size(a.bytes)}</td>
              <td class="nowrap">{shape(a)}</td>
              <td class="nowrap">{formatDate(a.uploaded_at)}</td>
              <td>{a.referenced_by || '—'}</td>
              <td class="right">
                <button type="button" class="danger" disabled={busy} onclick={() => remove(a)}>delete</button>
              </td>
            </tr>
          {:else}
            <tr><td colspan="7" class="hint">{loading ? 'loading…' : 'No assets yet. Upload one above, then use it in a media clip.'}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  </section>
</div>

<style>
  .page { width: min(1100px, 100% - 2rem); margin: 1rem auto 3rem; display: flex; flex-direction: column; gap: 1rem; }
  .subbar .meta { color: var(--dim); font-size: .7rem; }
  .note { color: var(--green); font-size: .74rem; }
  .err { color: var(--red); font-size: .8rem; margin: 0; }
  .grow { flex: 1; }
  .upload { display: flex; align-items: center; gap: .5rem; flex-wrap: wrap; }
  .upload input[type='file'] { max-width: 100%; font-size: .78rem; color: var(--muted); }
  .upload input[type='text'] { min-width: 12rem; flex: 1; max-width: 22rem; }
  .hint { margin: .6rem 0 0; color: var(--dim); font-size: .72rem; }
  .scroll { overflow-x: auto; }
  .name { max-width: 22rem; overflow-wrap: anywhere; }
  .nowrap { white-space: nowrap; }
  .right { text-align: right; }
  td.right button { font-size: .7rem; padding: .18rem .5rem; }

  @media (max-width: 700px) {
    .upload input[type='text'] { max-width: none; }
  }
</style>
