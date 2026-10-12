import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import { useUpdateChannel, useUpdateChannelStore } from '@/lib/betaFeatures';
import { LOCAL_SERVER_URL } from '@/lib/serverAddress';

interface ServerStore {
  customModelsDir: string | null;
  setCustomModelsDir: (dir: string | null) => void;
  /** Beta `network`: run this Mac's server for other Macs on the network. */
  shareOnNetwork: boolean;
  setShareOnNetwork: (share: boolean) => void;
  /** Beta `network`: use another Mac's server instead of this one's. */
  remoteServerUrl: string | null;
  setRemoteServerUrl: (url: string | null) => void;
}

export const useServerStore = create<ServerStore>()(
  persist(
    (set) => ({
      customModelsDir: null,
      setCustomModelsDir: (dir) => set({ customModelsDir: dir }),
      shareOnNetwork: false,
      setShareOnNetwork: (share) => set({ shareOnNetwork: share }),
      remoteServerUrl: null,
      setRemoteServerUrl: (url) => set({ remoteServerUrl: url }),
    }),
    {
      name: 'kass-server',
      partialize: (state) => ({
        customModelsDir: state.customModelsDir,
        shareOnNetwork: state.shareOnNetwork,
        remoteServerUrl: state.remoteServerUrl,
      }),
    },
  ),
);

/**
 * Another Mac's server this one uses, or null to use its own. Only for beta
 * users (`network` in lib/betaFeatures.ts), so null until the channel is read.
 */
export function remoteServerUrl(): string | null {
  if (useUpdateChannelStore.getState().channel !== 'beta') return null;
  return useServerStore.getState().remoteServerUrl;
}

/** Whether this Mac's server listens for other Macs (beta `network`). */
export function sharesOnNetwork(): boolean {
  if (useUpdateChannelStore.getState().channel !== 'beta') return false;
  return useServerStore.getState().shareOnNetwork && remoteServerUrl() === null;
}

/** The server every request goes to: this Mac's, or another's for beta users. */
export function serverUrl(): string {
  return remoteServerUrl() ?? LOCAL_SERVER_URL;
}

/** {@link serverUrl}, updating once the channel is read or the setting changes. */
export function useServerUrl(): string {
  const channel = useUpdateChannel();
  const remote = useServerStore((state) => state.remoteServerUrl);
  return (channel === 'beta' && remote) || LOCAL_SERVER_URL;
}
