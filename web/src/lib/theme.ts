// The one bit of the shared look CSS cannot do alone: WebKit has no way to
// colour a slider's track up to its thumb, so each slider carries the fill as
// --p, kept up to date here for every range input on the page -- including
// ones a page draws after load, and ones its own script moves.
// (Port of the Python app's theme.js; behaviour unchanged.)

const fill = (el: HTMLInputElement): void => {
  const min = Number(el.min || 0);
  const max = Number(el.max || 100);
  const p = max > min ? ((Number(el.value) - min) / (max - min)) * 100 : 0;
  el.style.setProperty('--p', `${Math.min(100, Math.max(0, p))}%`);
};

const isRange = (t: EventTarget | null): t is HTMLInputElement =>
  t instanceof HTMLInputElement && t.type === 'range';

/** Returns a function that undoes the installation. */
export function installRangeFill(): () => void {
  const all = () => document.querySelectorAll<HTMLInputElement>('input[type=range]').forEach(fill);
  const onInput = (e: Event) => {
    if (isRange(e.target)) fill(e.target);
  };
  document.addEventListener('input', onInput, true);
  document.addEventListener('change', onInput, true);
  const observer = new MutationObserver(all);
  observer.observe(document.documentElement, { childList: true, subtree: true });
  // A script setting .value does not fire an event; catch up now and then.
  const timer = setInterval(all, 500);
  all();
  return () => {
    document.removeEventListener('input', onInput, true);
    document.removeEventListener('change', onInput, true);
    observer.disconnect();
    clearInterval(timer);
  };
}
