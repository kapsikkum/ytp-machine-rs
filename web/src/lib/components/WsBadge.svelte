<script lang="ts">
  import { wsStatus } from '#lib/ws.ts';

  const label = { idle: 'offline', connecting: 'connecting…', open: 'live', reconnecting: 'reconnecting…' } as const;
</script>

<span class="ws" class:live={$wsStatus === 'open'} title="Live events from the server (/api/ws)">
  <i></i>{label[$wsStatus]}
</span>

<style>
  .ws { display: inline-flex; align-items: center; gap: .35rem; color: var(--dim); font-size: .7rem; white-space: nowrap; }
  i { width: 6px; height: 6px; border-radius: 50%; background: var(--amber); }
  .live i { background: var(--green); }
</style>
