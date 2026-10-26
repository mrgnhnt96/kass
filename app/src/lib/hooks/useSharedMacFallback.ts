import { useQuery } from '@tanstack/react-query';
import { invoke } from '@tauri-apps/api/core';
import { useEffect } from 'react';
import { queryClient } from '@/lib/queryClient';
import { answersAsKass, LOCAL_SERVER_URL } from '@/lib/serverAddress';
import { useRemoteServerUrl, useServerStore } from '@/stores/serverStore';

/** How often the main window checks whether the other Mac's Kass is up. */
const CHECK_MS = 3000;
/** How long it has to answer. Macs on one network answer in milliseconds. */
const CHECK_TIMEOUT_MS = 1500;

/**
 * Beta `network`: while using another Mac's Kass, check it every few seconds.
 * When it doesn't answer, this Mac's own server stands in: the app's requests
 * go there, its models load, and new takes start there. When it's back,
 * everything goes back to it; this Mac's server sends it what was done
 * meanwhile (backend/services/sync).
 */
export function useSharedMacFallback(): void {
  const remote = useRemoteServerUrl();

  useEffect(() => {
    if (!remote) {
      setAway(false);
      return;
    }
    let stopped = false;
    // This Mac's server may still be starting when the other Mac is first
    // found away, so ask it to load its models until it has.
    let woken = false;
    const check = async () => {
      const up = await answersAsKass(remote, CHECK_TIMEOUT_MS);
      if (stopped) return;
      setAway(!up);
      if (up) {
        woken = false;
      } else if (!woken) {
        woken = await wake();
      }
    };
    void check();
    const interval = setInterval(check, CHECK_MS);
    return () => {
      stopped = true;
      clearInterval(interval);
    };
  }, [remote]);
}

function setAway(away: boolean): void {
  const store = useServerStore.getState();
  if (store.sharedAway === away) return;
  store.setSharedAway(away);
  invoke('dictation_set_away', { away }).catch((error) =>
    console.warn('[network] dictation_set_away failed:', error),
  );
  console.log(
    away
      ? '[network] The other Mac is away; dictating on this one'
      : '[network] The other Mac is back',
  );
  // Everything shown came from the other server.
  void queryClient.invalidateQueries();
}

/** Ask this Mac's server to load its models; whether it answered. */
async function wake(): Promise<boolean> {
  try {
    const response = await fetch(`${LOCAL_SERVER_URL}/sync/wake`, { method: 'POST' });
    return response.ok;
  } catch {
    return false;
  }
}

export interface SharedMacSync {
  /** The other Mac can't be reached; this one stands in. */
  away: boolean;
  /** Changes made here that the other Mac doesn't have yet. */
  pending: number;
  /** Model downloads this Mac's server is fetching, to stand in without waiting. */
  downloading: string[];
  remote: string;
}

interface SyncStatusResponse {
  standing_in: boolean;
  pending?: number;
  downloading?: string[];
}

/** How syncing with the other Mac's Kass goes, or null when not using one. */
export function useSharedMacSync(): SharedMacSync | null {
  const remote = useRemoteServerUrl();
  const away = useServerStore((state) => state.sharedAway);
  const { data } = useQuery({
    queryKey: ['shared-mac-sync'],
    queryFn: async (): Promise<SyncStatusResponse> => {
      const response = await fetch(`${LOCAL_SERVER_URL}/sync/status`);
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      return response.json();
    },
    enabled: remote !== null,
    refetchInterval: 5000,
    retry: false,
  });
  if (!remote) return null;
  return {
    away,
    pending: data?.standing_in ? (data.pending ?? 0) : 0,
    downloading: data?.downloading ?? [],
    remote,
  };
}
