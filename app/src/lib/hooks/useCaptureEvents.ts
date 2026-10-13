import { useQueryClient } from '@tanstack/react-query';
import { useEffect } from 'react';
import { useUpdateChannel } from '@/lib/betaFeatures';
import { useServerStore, useServerUrl } from '@/stores/serverStore';

/** Changes close together (a take's transcript, then its cleanup) refetch once. */
const REFETCH_DELAY_MS = 250;

/**
 * Refetch captures and Insights when the server says they changed (beta
 * `network`). Captures dictated on another Mac sharing this server never
 * send this Mac's Tauri `capture:*` events, so without this they wait for
 * the cache to go stale.
 *
 * Only while sharing or using another Mac: alone, the Tauri events say it all.
 * EventSource reconnects by itself, and the server greets each connection
 * with `changed`, so whatever was missed while it was down is fetched.
 *
 * Call once from the main app shell.
 */
export function useCaptureEvents(ready: boolean) {
  const queryClient = useQueryClient();
  const beta = useUpdateChannel() === 'beta';
  const shared = useServerStore((state) => state.shareOnNetwork || state.remoteServerUrl !== null);
  const serverUrl = useServerUrl();
  const active = ready && beta && shared;

  useEffect(() => {
    if (!active) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const source = new EventSource(`${serverUrl}/captures/events`);
    source.addEventListener('changed', () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        queryClient.invalidateQueries({ queryKey: ['captures'] });
      }, REFETCH_DELAY_MS);
    });
    return () => {
      clearTimeout(timer);
      source.close();
    };
  }, [active, serverUrl, queryClient]);
}
