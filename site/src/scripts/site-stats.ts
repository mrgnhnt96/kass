// Amplitude for the website. The key is the public, ingestion-only project key.
// The SDK is bundled and loaded with a dynamic import, so nothing loads when the
// visitor has turned analytics off on /docs/privacy/ or their browser sends
// Global Privacy Control. Session Replay is not part of the bundle.

type Amplitude = typeof import('@amplitude/analytics-browser');

declare global {
  interface Window {
    amplitude?: Amplitude;
  }
  interface Navigator {
    globalPrivacyControl?: boolean;
  }
}

const KEY = '15288b16e4a64d54978fa9d86adddad1';
const OFF = 'kass-analytics-off';

const storedOff = () => {
  try {
    return localStorage.getItem(OFF) === '1';
  } catch {
    return false;
  }
};
const store = (off: boolean) => {
  try {
    if (off) localStorage.setItem(OFF, '1');
    else localStorage.removeItem(OFF);
  } catch {
    // Without storage the choice lasts until the page is left.
  }
};
const gpc = () => navigator.globalPrivacyControl === true;

let off = storedOff();
let loaded = false;
const load = () => {
  if (loaded) {
    window.amplitude?.setOptOut(false);
    return;
  }
  loaded = true;
  void import('@amplitude/analytics-browser').then((amplitude) => {
    // The download button and docs search call window.amplitude.track.
    window.amplitude = amplitude;
    // `off` is read now, so a toggle made while the SDK was loading still counts.
    amplitude.init(KEY, { serverZone: 'US', autocapture: true, optOut: off });
  });
};

const render = () => {
  const button = document.querySelector<HTMLButtonElement>('[data-analytics-toggle]');
  const status = document.querySelector('[data-analytics-status]');
  if (!button || !status) return;
  button.hidden = gpc();
  button.textContent = off ? 'Turn website analytics on' : 'Turn website analytics off';
  status.textContent = gpc()
    ? 'Analytics is off because your browser sends Global Privacy Control.'
    : off
      ? 'Analytics is off in this browser.'
      : 'Analytics is on in this browser.';
};

document.addEventListener(
  'click',
  (event) => {
    if (!(event.target instanceof Element) || !event.target.closest('[data-analytics-toggle]')) return;
    off = !off;
    store(off);
    if (off) window.amplitude?.setOptOut(true);
    else load();
    render();
  },
  true,
);

if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', render);
else render();

if (!off && !gpc()) load();

export {};
