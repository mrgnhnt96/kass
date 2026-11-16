import { invoke } from '@tauri-apps/api/core';

/**
 * Open the onboarding window. Opened by the user (`/setup`, the ⌘K palette,
 * Settings › Features) it starts from the beginning; `resume` picks up the
 * step it was on, for the launch after a permission made Kass quit. The
 * native window keeps its own progress.
 */
export function openOnboarding({ resume = false }: { resume?: boolean } = {}) {
  invoke('open_onboarding', { resume }).catch((err) =>
    console.warn('[onboarding] open_onboarding failed:', err),
  );
}
