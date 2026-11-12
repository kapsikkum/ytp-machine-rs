<script lang="ts" module>
  export interface ResultWord {
    word: string;
    kind: 'found' | 'run' | 'spliced' | 'missing';
    start?: number;
  }
</script>

<script lang="ts">
  // The finished video, its words (found / spliced / missing), and again/download.
  let {
    src,
    words = [],
    summary = '',
    onagain
  }: { src: string; words?: ResultWord[]; summary?: string; onagain?: () => void } = $props();

  let video = $state<HTMLVideoElement>();
  let now = $state(-1);

  function jump(w: ResultWord) {
    if (video && w.kind !== 'missing' && w.start !== undefined) {
      video.currentTime = w.start;
      void video.play();
    }
  }
  function tick() {
    if (!video) return;
    const t = video.currentTime;
    let idx = -1;
    words.forEach((w, i) => {
      if (w.start !== undefined && w.start <= t) idx = i;
    });
    now = video.paused && t === 0 ? -1 : idx;
  }
</script>

<section class="card result">
  <!-- svelte-ignore a11y_media_has_caption -->
  <video bind:this={video} {src} controls playsinline ontimeupdate={tick} onpause={tick} onseeked={tick}></video>
  <div class="words">
    {#each words as w, i (i)}
      <button type="button" class="word {w.kind}" class:now={now === i} disabled={w.kind === 'missing'} onclick={() => jump(w)}>{w.word}</button>
    {/each}
  </div>
  <div class="legend">
    <span><i style="background:var(--blue)"></i>real phrase</span>
    <span><i style="background:var(--green)"></i>clip</span>
    <span><i style="background:var(--amber)"></i>spliced</span>
    <span><i style="background:var(--red)"></i>missing</span>
    <span>click a word to jump to it</span>
  </div>
  <div class="result-bar">
    <span class="summary">{summary}</span>
    <div class="acts2">
      {#if onagain}<button type="button" class="btn" title="Same words, picked again" onclick={onagain}>↻ again</button>{/if}
      <a class="btn primary" href={src} download>⤓ download</a>
    </div>
  </div>
</section>

<style>
  .result { padding: .75rem; overflow: hidden; animation: rise .35s var(--ease); }
  @keyframes rise { from { transform: translateY(8px); opacity: 0; } to { transform: none; opacity: 1; } }
  video { display: block; width: 100%; border-radius: 8px; background: #000; aspect-ratio: 16 / 9; }
  .words { display: flex; flex-wrap: wrap; gap: .3rem; padding: .8rem .15rem .2rem; }
  .word {
    font-size: .8rem; padding: .28rem .55rem; border-radius: 7px; cursor: pointer;
    border: 1px solid var(--line-2); background: var(--raised); color: var(--muted);
  }
  .word:hover:not(:disabled) { transform: translateY(-1px); }
  .word.found { color: var(--green); border-color: #1d4d39; }
  .word.run { color: var(--blue); border-color: #164560; }
  .word.spliced { color: var(--amber); border-color: #5a4210; }
  .word.missing { color: var(--red); border-color: #5a1f1f; cursor: default; opacity: 1; }
  .word.now { background: var(--yellow); color: #000; border-color: var(--yellow); box-shadow: 0 0 0 3px rgba(255, 229, 0, .15); }
  .result-bar { display: flex; align-items: center; justify-content: space-between; gap: .6rem; flex-wrap: wrap; padding: .6rem .15rem .1rem; }
  .summary { color: var(--dim); font-size: .74rem; }
  .acts2 { display: flex; gap: .4rem; }
  .legend { display: flex; flex-wrap: wrap; gap: .8rem; padding: .3rem .15rem 0; color: var(--dim); font-size: .68rem; }
  .legend i { display: inline-block; width: 7px; height: 7px; border-radius: 50%; margin-right: .3rem; vertical-align: middle; }
</style>
