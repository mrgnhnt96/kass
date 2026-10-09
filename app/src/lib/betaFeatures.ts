import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useEffect } from 'react';
import { create } from 'zustand';

/**
 * Features that ship in every release but only show for beta users
 * (Settings › General › Beta updates). Add a name here and gate the feature
 * with `useBetaFeature`. To make it public, remove the name: every place
 * that still checks it stops compiling, which shows what to clean up.
 *
 * The server keeps its own list in backend/beta.py.
 */
export const BETA_FEATURES = [] as const;

export type BetaFeature = (typeof BETA_FEATURES)[number];

export type UpdateChannel = 'stable' | 'beta';

interface UpdateChannelStore {
  /** Null until read from the app. */
  channel: UpdateChannel | null;
  setChannel: (channel: UpdateChannel) => void;
}

export const useUpdateChannelStore = create<UpdateChannelStore>()((set) => ({
  channel: null,
  setChannel: (channel) => set({ channel }),
}));

let loaded = false;

/**
 * Read the channel once per window, then follow `update:channel`, which the
 * app sends to every window when the setting changes.
 */
function loadChannel() {
  if (loaded) return;
  loaded = true;
  const { setChannel } = useUpdateChannelStore.getState();
  listen<UpdateChannel>('update:channel', ({ payload }) => setChannel(payload)).catch((err) =>
    console.warn('[beta] listen failed:', err),
  );
  invoke<UpdateChannel>('update_channel')
    .then(setChannel)
    .catch(() => setChannel('stable'));
}

/** The update channel, loading it on first use. */
export function useUpdateChannel(): UpdateChannel | null {
  useEffect(loadChannel, []);
  return useUpdateChannelStore((state) => state.channel);
}

/** Whether a beta feature shows: only for beta users. */
export function useBetaFeature(_feature: BetaFeature): boolean {
  return useUpdateChannel() === 'beta';
}
