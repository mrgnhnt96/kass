import { useQueryClient } from '@tanstack/react-query';
import { listen } from '@tauri-apps/api/event';
import { useEffect, useState } from 'react';
import { useCaptureSettings } from '@/lib/hooks/useSettings';
import { usePlatform } from '@/platform/PlatformContext';
import { router } from '@/router';

/**
 * While onboarding isn't finished (the app opens it at launch, src/shell/
 * data.rs), take things back when it closes: every query is refetched (the onboarding window
 * changed settings and permissions this window hasn't seen), and "Show me
 * now" opens its page here.
 *
 * Returns whether onboarding owns the chord, which the main window's chord
 * sync pauses for.
 */
export function useOnboardingLauncher(): boolean {
  const platform = usePlatform();
  const queryClient = useQueryClient();
  const { settings } = useCaptureSettings();
  const [finished, setFinished] = useState(false);
  const pending =
    platform.metadata.isTauri && !finished && (!settings || !settings.onboarding_completed);

  useEffect(() => {
    if (!platform.metadata.isTauri) return;
    let disposed = false;
    let release: (() => void) | null = null;
    listen<{ show: string | null }>('onboarding:finished', ({ payload }) => {
      setFinished(true);
      queryClient.invalidateQueries();
      if (payload.show) router.navigate({ to: payload.show });
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else release = unlisten;
      })
      .catch((err) => console.warn('[onboarding] finished listener failed:', err));
    return () => {
      disposed = true;
      release?.();
    };
  }, [platform.metadata.isTauri, queryClient]);

  return pending;
}
