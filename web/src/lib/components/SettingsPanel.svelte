<script lang="ts">
  import { onMount } from 'svelte';
  import { api, errorText } from '#lib/api.ts';
  import { formatDate } from '#lib/store.ts';
  import type { BotsReport, EncoderReport } from '#lib/types.ts';

  // What the server can actually encode with, and whether the chat bots are
  // up. Read-only: the tokens and passwords behind the bots never leave the
  // server, so this panel only shows the probes and the non-secret detail.
  let encoders = $state<EncoderReport | null>(null);
  let bots = $state<BotsReport | null>(null);
  let loading = $state(true);
  let error = $state('');

  async function load() {
    loading = true;
    error = '';
    try {
      const [e, b] = await Promise.all([api.admin.encoders(), api.admin.bots()]);
      encoders = e;
      bots = b;
    } catch (err) {
      error = errorText(err);
    } finally {
      loading = false;
    }
  }

  const detail = (d: Record<string, unknown>) =>
    Object.entries(d)
      .map(([k, v]) => `${k}: ${typeof v === 'string' ? v : JSON.stringify(v)}`)
      .join(' · ');

  onMount(() => {
    void load();
  });
</script>

<section class="panel">
  <div class="head">
    <h2>settings</h2>
    <button type="button" onclick={load} disabled={loading}>⟲ refresh</button>
  </div>

  {#if error}<p class="err msg" role="alert">{error}</p>{/if}

  {#if loading}
    <p class="hint">reading the server…</p>
  {:else if encoders}
    <h3>encoding</h3>
    <p class="line">
      ffmpeg <code>{encoders.ffmpeg}</code>
      <span class="tag" class:good={encoders.ok} class:bad={!encoders.ok}>
        {encoders.ok ? 'ready' : 'not working'}
      </span>
      <span class="hint">{encoders.presets} preset{encoders.presets === 1 ? '' : 's'}</span>
    </p>
    <div class="scroll">
      <table class="data">
        <thead><tr><th>encoder</th><th>state</th><th>detail</th></tr></thead>
        <tbody>
          {#each encoders.encoders as e (e.encoder)}
            <tr>
              <td>{e.encoder}</td>
              <td><span class="tag" class:good={e.available} class:bad={!e.available}>{e.available ? 'available' : 'unavailable'}</span></td>
              <td class="detail">{e.detail}</td>
            </tr>
          {:else}
            <tr><td colspan="3" class="hint">No encoders reported.</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}

  {#if !loading && bots}
    <h3>chat bots</h3>
    <p class="line">service user <code>{bots.service_user ?? 'none'}</code></p>
    {#if bots.bots.length}
      <div class="bots">
        {#each bots.bots as b (b.platform)}
          <div class="bot">
            <div class="who">
              <b>{b.platform}</b>
              <span class="tag" class:good={b.connected} class:warn={b.enabled && !b.connected} class:bad={!b.enabled}>
                {b.connected ? 'connected' : b.enabled ? 'enabled, not connected' : 'off'}
              </span>
              {#if b.since}<span class="hint">since {formatDate(b.since)}</span>{/if}
            </div>
            {#if b.last_error}<p class="err">{b.last_error}</p>{/if}
            {#if Object.keys(b.detail).length}<p class="detail">{detail(b.detail)}</p>{/if}
          </div>
        {/each}
      </div>
    {:else}
      <p class="hint">No bot is configured.</p>
    {/if}
  {/if}
</section>

<style>
  .head { display: flex; align-items: center; gap: .75rem; margin-bottom: .75rem; }
  .head h2 { flex: 1; margin: 0; }
  h3 { font-size: .68rem; letter-spacing: 1.2px; text-transform: uppercase; color: var(--dim); font-weight: 500; margin: 1rem 0 .55rem; }
  h3:first-of-type { margin-top: 0; }
  .line { display: flex; align-items: center; flex-wrap: wrap; gap: .45rem; margin: 0 0 .5rem; font-size: .78rem; }
  code { color: var(--text); font-size: .74rem; }
  .scroll { overflow-x: auto; }
  .detail { color: var(--dim); font-size: .7rem; max-width: 34rem; overflow-wrap: anywhere; }
  .msg { font-size: .78rem; margin-bottom: .5rem; }
  .err { color: var(--red); font-size: .74rem; margin: .2rem 0; overflow-wrap: anywhere; }
  .bots { display: flex; flex-direction: column; gap: .55rem; }
  .bot { border-top: 1px solid var(--line); padding-top: .55rem; }
  .bot:first-child { border-top: 0; padding-top: 0; }
  .who { display: flex; align-items: center; gap: .45rem; flex-wrap: wrap; font-size: .78rem; }
  .who b { font-weight: 500; }
</style>
