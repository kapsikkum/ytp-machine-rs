<script lang="ts">
  let {
    status = '',
    statusError = false,
    busy = false,
    onfile
  }: { status?: string; statusError?: boolean; busy?: boolean; onfile: (f: File) => void } = $props();

  let input = $state<HTMLInputElement>();
  let over = $state(false);

  function pick(files: FileList | null) {
    const f = files?.[0];
    if (f && !busy) onfile(f);
    if (input) input.value = '';
  }
</script>

<div class="card wbcard">
  <div class="lede"><b>YTPMV</b><span>a MIDI file, sung by the corpus — every note pitch perfect</span></div>
  <div
    class="drop"
    class:over
    class:busy
    tabindex="0"
    role="button"
    aria-label="Choose or drop a MIDI file"
    aria-busy={busy}
    onclick={() => input?.click()}
    onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), input?.click())}
    ondragover={(e) => { e.preventDefault(); over = true; }}
    ondragleave={() => (over = false)}
    ondrop={(e) => { e.preventDefault(); over = false; pick(e.dataTransfer?.files ?? null); }}
  >
    <b>drop a .mid file here</b> or click to pick one
    <small>each instrument becomes a tile; each note replays its clip, tuned to the note · up to 2 MB</small>
  </div>
  <input bind:this={input} type="file" accept=".mid,.midi,audio/midi,audio/x-midi" hidden onchange={(e) => pick(e.currentTarget.files)} />
  <div class="hint status" class:err={statusError} role="status">{status}</div>
</div>

<style>
  .wbcard { width: 100%; padding: 1.25rem; border-radius: 0; margin: 0; }
  .lede { color: var(--muted); font-size: .8rem; margin-bottom: .9rem; }
  .lede b { font-family: Impact, 'Arial Black', sans-serif; font-weight: 400; letter-spacing: 1px; color: var(--yellow); font-size: 1.05rem; margin-right: .5rem; }
  .drop {
    border: 2px dashed var(--line-2); border-radius: 10px; padding: 2.25rem 1rem;
    text-align: center; color: var(--muted); cursor: pointer;
    transition: border-color .15s, color .15s, background .15s;
  }
  .drop:hover, .drop.over { border-color: var(--yellow); color: var(--text); background: rgba(255, 229, 0, .04); }
  .drop.busy { opacity: .6; cursor: progress; }
  .drop b { color: var(--yellow); font-weight: 500; }
  .drop small { display: block; margin-top: .5rem; color: var(--dim); font-size: .72rem; }
  .status:empty { display: none; }
  .hint { margin-top: .75rem; }
</style>
