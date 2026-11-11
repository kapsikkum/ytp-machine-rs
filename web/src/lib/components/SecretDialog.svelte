<script lang="ts">
  // Shows an API token secret exactly once. The server never returns it again.
  let { name, secret, onclose }: { name: string; secret: string; onclose: () => void } = $props();

  let copied = $state(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(secret);
      copied = true;
    } catch {
      copied = false;
    }
  }
</script>

<div class="scrim" role="presentation">
  <div class="dialog" role="alertdialog" aria-modal="true" aria-labelledby="secret-title">
    <div class="head">
      <h3 id="secret-title">token “{name}” created</h3>
    </div>
    <p class="hint"><b>Copy it now.</b> This is the only time the secret is shown; the server keeps just a hash of it.</p>
    <code class="secret" tabindex="-1">{secret}</code>
    <div class="actions">
      <button class="primary" onclick={copy}>{copied ? 'copied ✓' : 'copy'}</button>
      <button onclick={onclose}>I've saved it</button>
    </div>
  </div>
</div>

<style>
  .scrim { position: fixed; inset: 0; background: rgba(0, 0, 0, .7); z-index: 100; display: flex; align-items: center; justify-content: center; }
  .dialog {
    background: #18181b; border: 1px solid var(--line-2); border-radius: 12px; padding: 1.25rem;
    width: min(92vw, 520px); box-shadow: 0 20px 40px rgba(0, 0, 0, .6); display: flex; flex-direction: column; gap: .9rem;
  }
  h3 { font-size: 1rem; font-weight: 600; color: var(--text); }
  .secret {
    display: block; background: var(--bg); border: 1px dashed var(--yellow); border-radius: 8px;
    padding: .7rem .8rem; color: var(--yellow); font-size: .85rem; overflow-wrap: anywhere; user-select: all;
  }
  .actions { display: flex; justify-content: flex-end; gap: .5rem; }
</style>
