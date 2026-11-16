import { useQuery } from '@tanstack/react-query';
import { LOCAL_SERVER_URL } from '@/lib/serverAddress';
import { useRemoteServerUrl, useServerStore } from '@/stores/serverStore';

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

/**
 * How syncing with the other Mac's Kass goes, or null when not using one.
 * The app checks whether that Mac is away (src/shell/data.rs `check_away`).
 */
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
