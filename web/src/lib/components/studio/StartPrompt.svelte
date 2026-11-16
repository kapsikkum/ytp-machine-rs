<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import { api, errorText } from '#lib/api.ts';
  import { can } from '#lib/auth.ts';
  import { selectedVoice } from '#lib/voice.ts';

  // The Studio's own prompt. This is the plan the Say page saves, not a render
  // job: it writes the sentence into a project and opens that project here, so
  // the next thing you do is edit the timeline, not wait for a video.
  let prompt = $state(page.url.searchParams.get('prompt') ?? '');
  let busy = $state(false);
  let error = $state('');
  const allowed = $derived($can('generate.ytp') && $can('project.write'));

  async function submit() {
    if (!allowed || busy || !prompt.trim()) return;
    busy = true;
    error = '';
    try {
      const r = await api.ytp.plan({
        text: prompt.trim(),
        voice: $selectedVoice || undefined,
        save: true
      });
      if (!r.id) throw new Error('the plan was saved without a project id');
      await goto(`/?project=${r.id}`);
    } catch (e) {
      error = errorText(e);
    } finally {
      busy = false;
    }
  }

  function create(e: SubmitEvent) {
    e.preventDefault();
    void submit();
  }

  function keydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void submit();
    }
  }
</script>

<section class="card prompt-card">
  <div class="section-label">start from a prompt</div>

  {#if allowed}
    <form onsubmit={create}>
      <textarea rows="3" bind:value={prompt} maxlength="4000" spellcheck="false" disabled={busy}
                placeholder="type something for the voice to say…"
                aria-label="Prompt for a new project"
                onkeydown={keydown}></textarea>
      <p class="hint">Writes the sentence as an editable project. Nothing is rendered until you press render.</p>
      {#if error}<p class="error" role="alert">{error}</p>{/if}
      <div class="row">
        <button type="submit" class="primary" disabled={busy || !prompt.trim()}>
          {busy ? 'planning…' : 'create project'}
        </button>
        <a class="ghost" href="/ytpmv">make a song</a>
      </div>
    </form>
  {:else}
    <p class="hint">Creating a project needs generate.ytp and project.write.</p>
    <div class="row">
      <a class="ghost" href="/ytp">open the Say page</a>
      <a class="ghost" href="/ytpmv">make a song</a>
    </div>
  {/if}
</section>

<style>
  .prompt-card { padding: 1rem; display: flex; flex-direction: column; gap: .75rem; }
  form { display: flex; flex-direction: column; gap: .6rem; }
  textarea {
    width: 100%; resize: vertical; min-height: 5.5rem; font-size: .95rem; line-height: 1.45;
    padding: .55rem .65rem; caret-color: var(--yellow);
  }
  .hint { margin: 0; color: var(--dim); font-size: .72rem; }
  .error { margin: 0; color: var(--red); font-size: .76rem; }
  .row { display: flex; align-items: center; gap: .5rem; flex-wrap: wrap; }
</style>
