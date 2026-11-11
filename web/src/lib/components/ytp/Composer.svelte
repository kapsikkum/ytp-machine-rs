<script lang="ts">
  import type { Snippet } from 'svelte';
  import { DEFAULTS, chipsOf, type Options } from '#lib/ytp/options.ts';
  import Tweaks from './Tweaks.svelte';

  let {
    text = $bindable(),
    opts = $bindable(),
    captions = $bindable(),
    tweaksOpen = $bindable(),
    busy = false,
    status = '',
    statusError = false,
    progress = null,
    onsubmit,
    actions
  }: {
    text: string;
    opts: Options;
    captions: boolean;
    tweaksOpen: boolean;
    busy?: boolean;
    status?: string;
    statusError?: boolean;
    /** 0..1, or null for none; -1 for indeterminate */
    progress?: number | null;
    onsubmit: () => void;
    actions?: Snippet;
  } = $props();

  const chips = $derived(chipsOf(text));
  const changed = $derived(Object.keys(DEFAULTS).some((k) => opts[k] !== DEFAULTS[k]));
  const MAX_CHIPS = 60;
  let showAll = $state(false);
  const shownChips = $derived(showAll ? chips : chips.slice(0, MAX_CHIPS));

  function keydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      if (text.trim() && !busy) onsubmit();
    }
  }

  function grow(el: HTMLTextAreaElement) {
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, window.innerHeight * 0.5) + 'px';
  }
  const autosize = (el: HTMLTextAreaElement) => {
    $effect(() => {
      void text;
      grow(el);
    });
  };
</script>

<section class="card composer">
  <form
    autocomplete="off"
    onsubmit={(e) => {
      e.preventDefault();
      if (text.trim() && !busy) onsubmit();
    }}
  >
    <div class="input-wrap">
      <textarea
        id="text-input"
        rows="2"
        spellcheck="false"
        aria-label="What should be said"
        placeholder="type something for the voice to say…"
        bind:value={text}
        onkeydown={keydown}
        use:autosize
      ></textarea>
    </div>
    <div class="chips" aria-live="polite">
      {#each shownChips as c, i (i)}
        <span class="chip" class:fx={c.reversed}>{c.word}{#if c.reversed}<span class="fxmark" title="played backwards"> ⟲</span>{/if}</span>
      {/each}
      {#if chips.length > MAX_CHIPS && !showAll}
        <button type="button" class="chip more" onclick={() => (showAll = true)}>+{chips.length - MAX_CHIPS} more</button>
      {/if}
    </div>

    <div class="bar">
      <div class="left">
        <button type="button" class="ghost tweak" aria-expanded={tweaksOpen} aria-controls="tweaks" onclick={() => (tweaksOpen = !tweaksOpen)}>
          <span aria-hidden="true">⚙</span> tweak <span class="dot" class:shown={changed} title="changed from the defaults"></span>
        </button>
        <label class="switch" title="Burn each word onto the picture as it is said">
          <input type="checkbox" bind:checked={captions} /><span class="track"></span> captions
        </label>
        <span class="meter">{#if chips.length}<b>{chips.length}</b> word{chips.length === 1 ? '' : 's'}{/if}</span>
      </div>
      <div class="right">
        {@render actions?.()}
        <button type="submit" class="go" disabled={busy || !text.trim()}>SAY IT<kbd>Ctrl ↵</kbd></button>
      </div>
    </div>

    <Tweaks bind:opts open={tweaksOpen} />

    <div class="helpers">
      <span><button type="button" onclick={() => (text = (text.trimEnd() + ' ~word~').trimStart())}>~backwards~</button></span>
      <span><button type="button" onclick={() => (text = (text.trimEnd() + ' sooooo').trimStart())}>loooong</button> holds a letter</span>
    </div>
  </form>

  {#if progress !== null}
    <div class="progress" class:indeterminate={progress < 0}>
      <div class="track"><div class="progress-bar" style:width={progress >= 0 ? `${Math.round(progress * 100)}%` : undefined}></div></div>
    </div>
  {/if}
  <div class="status-row"><div class="status" class:error={statusError} role="status">{status}</div></div>
</section>

<style>
  .composer { padding: 1rem; }
  .input-wrap { position: relative; }
  #text-input {
    display: block; width: 100%;
    background: transparent; border: none; resize: none;
    font-size: clamp(1.15rem, 3.2vw, 1.5rem); line-height: 1.45;
    padding: .5rem .35rem .25rem; min-height: 5.5rem; max-height: 50vh;
    caret-color: var(--yellow);
  }
  #text-input:focus-visible { outline: none; }
  #text-input::placeholder { color: #3e3e45; }

  .chips { display: flex; flex-wrap: wrap; gap: .3rem; padding: .35rem .35rem .75rem; min-height: 1.9rem; }
  .chip {
    display: inline-flex; align-items: center; gap: .3rem;
    font-size: .74rem; line-height: 1; padding: .3rem .5rem; border-radius: 999px;
    border: 1px solid var(--line-2); color: var(--muted); background: var(--raised);
    animation: pop .18s var(--ease); white-space: nowrap;
  }
  .chip.fx { color: var(--violet); border-color: #463763; background: #1a1526; }
  .chip.more { color: var(--dim); border-style: dashed; background: transparent; cursor: pointer; font: inherit; font-size: .74rem; }
  .chip.more:hover { color: var(--text); }
  @keyframes pop { from { transform: scale(.85); opacity: 0; } to { transform: none; opacity: 1; } }

  .bar {
    display: flex; align-items: center; justify-content: space-between; gap: .75rem; flex-wrap: wrap;
    border-top: 1px solid var(--line); padding-top: .75rem;
  }
  .left, .right { display: flex; align-items: center; gap: .5rem; flex-wrap: wrap; }
  .tweak { transition: color .15s, border-color .15s, background .15s; }
  .tweak[aria-expanded='true'] { color: #000; background: var(--yellow); border-color: var(--yellow); }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--amber); display: none; }
  .dot.shown { display: inline-block; }
  .tweak[aria-expanded='true'] .dot { background: #000; }
  .meter { color: var(--dim); font-size: .72rem; padding-left: .25rem; }
  .meter b { color: var(--muted); font-weight: 500; }

  .go {
    display: inline-flex; align-items: center; cursor: pointer;
    background: var(--yellow); color: #000; border: none; border-radius: 999px;
    font-family: Impact, 'Arial Black', sans-serif; font-size: 1.1rem; letter-spacing: 1.5px;
    padding: .6rem 1.4rem .55rem;
    box-shadow: 0 6px 20px rgba(255, 229, 0, .18);
    transition: transform .12s var(--ease), background .15s, box-shadow .15s, opacity .15s;
  }
  .go:hover:not(:disabled) { background: var(--yellow-2); box-shadow: 0 8px 26px rgba(255, 229, 0, .28); border-color: transparent; }
  .go:active { transform: scale(.97); }
  .go:disabled { opacity: .35; cursor: not-allowed; box-shadow: none; transform: none; }

  .helpers { display: flex; flex-wrap: wrap; align-items: center; gap: .35rem .6rem; padding: .8rem .35rem 0; color: var(--dim); font-size: .72rem; }
  .helpers button {
    cursor: pointer; background: none; border: 1px solid var(--line); border-radius: 6px;
    padding: .12rem .45rem; font-size: .72rem; color: var(--muted); transition: all .15s;
  }
  .helpers button:hover { color: var(--yellow); border-color: var(--yellow); background: none; }

  .progress { margin-top: .9rem; }
  .progress .track { height: 3px; background: var(--line); border-radius: 999px; overflow: hidden; }
  .progress-bar { height: 100%; width: 0; background: var(--yellow); border-radius: 999px; transition: width .35s var(--ease); }
  .progress.indeterminate .progress-bar { width: 30%; animation: slide 1.3s ease-in-out infinite; }
  @keyframes slide { from { transform: translateX(-100%); } to { transform: translateX(340%); } }
  .status-row { display: flex; align-items: center; justify-content: space-between; gap: .75rem; margin-top: .4rem; }
  .status { margin-top: .1rem; font-size: .76rem; color: var(--muted); min-height: 1.1em; }
  .status.error { color: var(--red); }

  @media (max-width: 640px) {
    .bar { flex-direction: column-reverse; align-items: stretch; }
    .right { flex-direction: column-reverse; align-items: stretch; }
    .go { justify-content: center; padding: .8rem; }
    .go :global(kbd) { display: none; }
  }
</style>
