<script lang="ts">
  import { clipLabel, hasPicture, type Effect, type Place, type ProjectClip } from '#lib/project.ts';

  // Effects of the selected clip (crates/ytp-core/src/timeline.rs, `Effect`).
  // The panel never invents a name the server would reject: the glitch list is
  // the renderer's vocabulary (crates/ytp-media/src/glitch.rs), and glitch
  // names already on the clip stay editable only through their own row.
  let {
    clip,
    editable = true,
    onchange,
    onplace,
    ongenerated
  }: {
    clip: ProjectClip;
    editable?: boolean;
    onchange: (effects: Effect[]) => void;
    /** Sets or clears the clip's picture box; omitted = the block is read-only-ish. */
    onplace?: (place: Place | undefined) => void;
    /** Sets a generated clip's prompt or seed; omitted = the block is read-only-ish. */
    ongenerated?: (field: 'prompt' | 'seed', value: string) => void;
  } = $props();

  type Numeric = 'speed' | 'stretch' | 'pitch' | 'volume' | 'fade_in' | 'fade_out' | 'pause';

  const NUMERIC: Record<Numeric, { name: string; unit: string; min: number; max: number; step: number; add: number }> = {
    speed: { name: 'speed', unit: 'x', min: 0.1, max: 10, step: 0.05, add: 2 },
    stretch: { name: 'stretch', unit: 'x', min: 0.1, max: 16, step: 0.05, add: 2 },
    pitch: { name: 'pitch', unit: 'semitones', min: -48, max: 48, step: 1, add: 12 },
    volume: { name: 'volume', unit: 'dB', min: -80, max: 40, step: 1, add: -6 },
    fade_in: { name: 'fade in', unit: 's', min: 0, max: 60, step: 0.05, add: 0.5 },
    fade_out: { name: 'fade out', unit: 's', min: 0, max: 60, step: 0.05, add: 0.5 },
    pause: { name: 'pause', unit: 's', min: 0, max: 60, step: 0.05, add: 0.5 }
  };

  const GLITCHES = ['shake', 'punch', 'spin', 'rainbow', 'invert', 'strobe', 'crunch', 'fringe', 'earrape', 'wobble', 'cave'];

  const FLAGS: { type: 'reverse' | 'stutter' | 'mirror' | 'subword'; label: string }[] = [
    { type: 'reverse', label: 'reverse' },
    { type: 'stutter', label: 'stutter' },
    { type: 'mirror', label: 'flip' },
    { type: 'subword', label: 'subword' }
  ];

  // Fixed boxes only: every value is a frame fraction in 0..=1 with w, h > 0,
  // x + w <= 1 and y + h <= 1, so picking one can never fail the server's
  // `Place::check` (crates/ytp-core/src/timeline.rs). No free numbers.
  const PRESETS: { key: string; label: string; place?: Place }[] = [
    { key: 'default', label: 'default' },
    { key: 'top-left', label: '↖ top left', place: { x: 0, y: 0, w: 0.5, h: 0.5 } },
    { key: 'top-right', label: '↗ top right', place: { x: 0.5, y: 0, w: 0.5, h: 0.5 } },
    { key: 'bottom-left', label: '↙ bottom left', place: { x: 0, y: 0.5, w: 0.5, h: 0.5 } },
    { key: 'bottom-right', label: '↘ bottom right', place: { x: 0.5, y: 0.5, w: 0.5, h: 0.5 } },
    { key: 'top', label: 'top strip', place: { x: 0, y: 0, w: 1, h: 0.5 } },
    { key: 'bottom', label: 'bottom strip', place: { x: 0, y: 0.5, w: 1, h: 0.5 } },
    { key: 'left', label: 'left strip', place: { x: 0, y: 0, w: 0.5, h: 1 } },
    { key: 'right', label: 'right strip', place: { x: 0.5, y: 0, w: 0.5, h: 1 } },
    { key: 'full', label: 'full', place: { x: 0, y: 0, w: 1, h: 1 } }
  ];

  const samePlace = (a: Place | undefined, b: Place | undefined) =>
    !a || !b
      ? !a && !b
      : (['x', 'y', 'w', 'h'] as const).every((k) => Math.abs(a[k] - b[k]) < 1e-9);

  // The preset the clip is on, or '' for a box that came from elsewhere.
  const placed = $derived(PRESETS.find((p) => samePlace(p.place, clip.place))?.key ?? '');

  function usePlace(p: { place?: Place }) {
    if (!editable || !onplace || samePlace(p.place, clip.place)) return;
    warn = '';
    onplace(p.place);
  }

  let warn = $state('');

  // A different clip starts with a clean warning line.
  $effect(() => {
    clip.id;
    warn = '';
  });

  const addable = $derived.by(() => {
    const types = new Set(clip.effects.map((e) => e.type));
    const present = new Set(clip.effects.filter((e) => e.type === 'glitch').map((e) => (e as { name: string }).name));
    return {
      numbers: (Object.keys(NUMERIC) as Numeric[]).filter((k) => !types.has(k)),
      flags: FLAGS.filter((f) => !types.has(f.type)),
      glitches: GLITCHES.filter((n) => !present.has(n))
    };
  });

  const empty = $derived(addable.numbers.length + addable.flags.length + addable.glitches.length === 0);

  const fmt = (v: number) => (Number.isInteger(v) ? String(v) : String(Math.round(v * 100) / 100));

  function isNumeric(t: Effect['type']): t is Numeric {
    return Object.prototype.hasOwnProperty.call(NUMERIC, t);
  }

  function numeric(e: Effect): { name: string; unit: string; min: number; max: number; step: number } | null {
    return isNumeric(e.type) ? NUMERIC[e.type] : null;
  }

  function value(e: Effect): number {
    switch (e.type) {
      case 'speed':
        return e.factor;
      case 'stretch':
        return e.factor;
      case 'pitch':
        return e.semitones;
      case 'volume':
        return e.db;
      case 'fade_in':
      case 'fade_out':
      case 'pause':
        return e.seconds;
      default:
        return 0;
    }
  }

  function withValue(e: Effect, v: number): Effect {
    switch (e.type) {
      case 'speed':
        return { type: 'speed', factor: v };
      case 'stretch':
        return { type: 'stretch', factor: v };
      case 'pitch':
        return { type: 'pitch', semitones: v };
      case 'volume':
        return { type: 'volume', db: v };
      case 'fade_in':
        return { type: 'fade_in', seconds: v };
      case 'fade_out':
        return { type: 'fade_out', seconds: v };
      case 'pause':
        return { type: 'pause', seconds: v };
      default:
        return e;
    }
  }

  function blank(k: Numeric): Effect {
    const v = NUMERIC[k].add;
    switch (k) {
      case 'speed':
        return { type: 'speed', factor: v };
      case 'stretch':
        return { type: 'stretch', factor: v };
      case 'pitch':
        return { type: 'pitch', semitones: v };
      case 'volume':
        return { type: 'volume', db: v };
      case 'fade_in':
        return { type: 'fade_in', seconds: v };
      case 'fade_out':
        return { type: 'fade_out', seconds: v };
      case 'pause':
        return { type: 'pause', seconds: v };
    }
  }

  function label(e: Effect): string {
    switch (e.type) {
      case 'mirror':
        return 'flip';
      case 'glitch':
        return `glitch ${e.name}`;
      case 'fade_in':
        return 'fade in';
      case 'fade_out':
        return 'fade out';
      default:
        return e.type;
    }
  }

  const accepts = (n: { min: number; max: number; unit: string }) => `${n.min} to ${n.max} ${n.unit}`;

  function edit(index: number, raw: string, el: HTMLInputElement) {
    if (!editable) return;
    const e = clip.effects[index];
    const n = e ? numeric(e) : null;
    if (!e || !n) return;
    const parsed = Number(raw);
    if (raw.trim() === '' || !Number.isFinite(parsed)) {
      warn = `${n.name} accepts ${accepts(n)}; “${raw}” is not a number, so ${fmt(value(e))} was kept.`;
      el.value = fmt(value(e));
      return;
    }
    const clamped = Math.min(n.max, Math.max(n.min, parsed));
    warn =
      clamped === parsed
        ? ''
        : `${n.name} accepts ${accepts(n)}; ${fmt(parsed)} was clamped to ${fmt(clamped)}.`;
    if (clamped !== parsed) el.value = fmt(clamped);
    if (clamped === value(e)) return;
    const next = clip.effects.slice();
    next[index] = withValue(e, clamped);
    onchange(next);
  }

  function add(e: Effect) {
    if (!editable) return;
    warn = '';
    onchange([...clip.effects, e]);
  }

  function remove(index: number) {
    if (!editable) return;
    warn = '';
    const next = clip.effects.slice();
    next.splice(index, 1);
    onchange(next);
  }

  // The server's limits for a generated clip (crates/ytp-core/src/timeline.rs):
  // a prompt of at most 4000 characters, and a seed of at most 64 bytes with no
  // control characters. The prompt's maxlength is in code units, the seed's is
  // trimmed by bytes; anything trimmed is written back so the field shows what
  // will be saved.
  function generated(field: 'prompt' | 'seed', raw: string, el: HTMLInputElement | HTMLTextAreaElement) {
    if (!editable || !ongenerated) return;
    const source = clip.source;
    if (source.type !== 'generated') return;
    const value = field === 'prompt' ? raw.slice(0, 4000) : seed(raw);
    if (value !== raw) el.value = value;
    if (value !== source[field]) ongenerated(field, value);
  }

  function seed(raw: string): string {
    let s = raw.replace(/[\u0000-\u001F\u007F-\u009F]/g, '');
    while (new TextEncoder().encode(s).length > 64) s = [...s].slice(0, -1).join('');
    return s;
  }
</script>

<section class="card effects" aria-label="Effects of the selected clip">
  <div class="head">
    <span class="section-label">effects</span>
    <b class="clip">{clipLabel(clip)}</b>
    <span class="count">{clip.effects.length ? `${clip.effects.length} applied` : 'none yet'}</span>
    {#if !editable}<span class="count">read-only</span>{/if}
  </div>

  {#if warn}
    <p class="warn" role="alert">{warn}</p>
  {/if}

  {#if clip.source.type === 'generated'}
    {@const g = clip.source}
    <div class="place">
      <div class="group">
        <span class="glabel">prompt</span>
        <textarea rows="3" maxlength="4000" disabled={!editable || !ongenerated}
                  aria-label="Prompt of the generated clip" placeholder="what this clip should say"
                  value={g.prompt}
                  onchange={(ev) => generated('prompt', ev.currentTarget.value, ev.currentTarget)}></textarea>
      </div>
      <div class="group">
        <span class="glabel">seed</span>
        <input type="text" maxlength="64" disabled={!editable || !ongenerated}
               aria-label="Seed of the generated clip" placeholder="empty = a new seed each time"
               value={g.seed}
               onchange={(ev) => generated('seed', ev.currentTarget.value, ev.currentTarget)} />
      </div>
      <p class="hint">the length is decided when it renders; later clips shift to fit</p>
      <p class="hint">renders from the project's corpus unless a corpus is set</p>
    </div>
  {/if}

  {#if clip.effects.length}
    <ul class="rows">
      {#each clip.effects as e, i (i)}
        {@const n = numeric(e)}
        <li class="row">
          <span class="name">{label(e)}</span>
          {#if n}
            <span class="value">{fmt(value(e))} <em>{n.unit}</em></span>
            <input
              type="number"
              min={n.min}
              max={n.max}
              step={n.step}
              value={fmt(value(e))}
              disabled={!editable}
              aria-label="{n.name}, {accepts(n)}"
              onchange={(ev) => edit(i, ev.currentTarget.value, ev.currentTarget)}
            />
          {:else}
            <span class="value flag">no parameter</span>
            <span></span>
          {/if}
          <button type="button" class="remove" disabled={!editable} onclick={() => remove(i)} aria-label="Remove {label(e)}">remove</button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="empty">This clip plays as it is.</p>
  {/if}

  {#if editable && !empty}
    <div class="adder">
      {#if addable.numbers.length}
        <div class="group">
          <span class="glabel">with a value</span>
          <div class="chips">
            {#each addable.numbers as k (k)}
              <button type="button" class="chip" onclick={() => add(blank(k))}>+ {NUMERIC[k].name}</button>
            {/each}
          </div>
        </div>
      {/if}
      {#if addable.flags.length}
        <div class="group">
          <span class="glabel">no parameter</span>
          <div class="chips">
            {#each addable.flags as f (f.type)}
              <button type="button" class="chip" onclick={() => add({ type: f.type } as Effect)}>+ {f.label}</button>
            {/each}
          </div>
        </div>
      {/if}
      {#if addable.glitches.length}
        <div class="group">
          <span class="glabel">glitch</span>
          <div class="chips">
            {#each addable.glitches as g (g)}
              <button type="button" class="chip glitch" onclick={() => add({ type: 'glitch', name: g })}>+ {g}</button>
            {/each}
          </div>
        </div>
      {/if}
    </div>
  {/if}

  {#if hasPicture(clip)}
    <div class="place">
      <div class="group">
        <span class="glabel">picture in picture</span>
        <div class="chips">
          {#each PRESETS as p (p.key)}
            <button type="button" class="chip" class:active={placed === p.key}
                    aria-pressed={placed === p.key}
                    disabled={!editable || !onplace}
                    onclick={() => usePlace(p)}>{p.label}</button>
          {/each}
        </div>
      </div>
      <p class="hint">the box only shows where this clip overlaps a clip on another track; no box = the default box, bottom-right half</p>
    </div>
  {/if}
</section>

<style>
  .effects { padding: .8rem 1rem 1rem; display: flex; flex-direction: column; gap: .6rem; }
  .head { display: flex; align-items: baseline; gap: .6rem; flex-wrap: wrap; }
  .head .section-label { margin: 0; }
  .head .clip { font-size: .8rem; font-weight: 500; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 40ch; }
  .head .count { color: var(--dim); font-size: .7rem; }
  .warn { margin: 0; color: var(--amber); font-size: .75rem; }

  .rows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; }
  .row { display: grid; grid-template-columns: minmax(7rem, 12rem) minmax(7rem, auto) 6.5rem auto; align-items: center; gap: .55rem .7rem; padding: .32rem 0; border-top: 1px solid var(--line); }
  .row:first-child { border-top: 0; }
  .name { color: var(--muted); font-size: .74rem; }
  .value { color: var(--text); font-size: .76rem; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .value em { color: var(--dim); font-style: normal; font-size: .7rem; }
  .value.flag { color: var(--dim); }
  .row input { width: 100%; padding: .2rem .45rem; font-size: .76rem; text-align: right; }
  .remove { font-size: .7rem; padding: .18rem .5rem; justify-self: end; }
  .empty { margin: 0; color: var(--dim); font-size: .76rem; }

  .adder { display: flex; flex-direction: column; gap: .45rem; border-top: 1px dashed var(--line); padding-top: .65rem; }
  .group { display: grid; grid-template-columns: 6.5rem minmax(0, 1fr); align-items: baseline; gap: .6rem; }
  .glabel { color: var(--dim); font-size: .66rem; letter-spacing: .5px; text-transform: uppercase; }
  .chips { display: flex; flex-wrap: wrap; gap: .3rem; }
  .chip { font-size: .7rem; padding: .18rem .5rem; border-radius: 999px; color: var(--muted); }
  .chip:hover:not(:disabled) { color: var(--text); }
  .chip.glitch { border-style: dashed; }
  .chip.active { color: var(--text); border-color: var(--muted); background: var(--line); }

  .place { display: flex; flex-direction: column; gap: .4rem; border-top: 1px dashed var(--line); padding-top: .65rem; }
  .hint { margin: 0; color: var(--dim); font-size: .7rem; }

  @media (max-width: 650px) {
    .row { grid-template-columns: minmax(0, 1fr) auto; }
    .row input { grid-column: 2; width: 6.5rem; }
    .group { grid-template-columns: minmax(0, 1fr); }
  }
</style>
