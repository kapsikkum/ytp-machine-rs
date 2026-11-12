// One audio element for the whole page: starting something stops what was playing.

let el: HTMLAudioElement | null = null;
let url: string | null = null;
let timer: ReturnType<typeof setInterval> | undefined;
let onEnd: (() => void) | null = null;

function release(): void {
  clearInterval(timer);
  if (el) {
    el.onended = null;
    el.pause();
  }
  if (url) URL.revokeObjectURL(url);
  url = null;
  const done = onEnd;
  onEnd = null;
  done?.();
}

export function stop(): void {
  release();
}

/** Play a WAV. `onTime` gets seconds into it as it plays; `done` fires once it ends or is replaced. */
export async function play(blob: Blob, onTime?: (seconds: number) => void, done?: () => void): Promise<void> {
  release();
  el ??= new Audio();
  url = URL.createObjectURL(blob);
  onEnd = done ?? null;
  el.src = url;
  el.onended = () => release();
  try {
    await el.play();
  } catch (e) {
    release();
    throw e;
  }
  timer = setInterval(() => {
    if (el) onTime?.(el.currentTime);
  }, 50);
}
