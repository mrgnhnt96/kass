import { RouterProvider } from '@tanstack/react-router';
import { useEffect, useRef, useState } from 'react';
import kassLogo from '@/assets/kass-logo.png';
import { DictateWindow } from '@/components/DictateWindow/DictateWindow';
import { OnboardingWindow } from '@/components/Onboarding/OnboardingWindow';
import { useOnboardingLauncher } from '@/components/Onboarding/useOnboardingLauncher';
import ShinyText from '@/components/ShinyText';
import { TitleBarDragRegion } from '@/components/TitleBarDragRegion';
import { useThemeSync } from '@/hooks/useThemeSync';
import { apiClient } from '@/lib/api/client';
import type { HealthResponse } from '@/lib/api/types';
import { readUpdateChannel } from '@/lib/betaFeatures';
import { TOP_SAFE_AREA_PADDING } from '@/lib/constants/ui';
import { useCaptureEvents } from '@/lib/hooks/useCaptureEvents';
import { useChordSync } from '@/lib/hooks/useChordSync';
import { useDeepLinks } from '@/lib/hooks/useDeepLinks';
import { useInAppDictationInsert } from '@/lib/hooks/useInAppDictationInsert';
import { useSoundCueSync } from '@/lib/hooks/useSoundCueSync';
import { queryClient } from '@/lib/queryClient';
import { cn } from '@/lib/utils/cn';
import { usePlatform } from '@/platform/PlatformContext';
import { router } from '@/router';
import { useLogStore } from '@/stores/logStore';
import { remoteServerUrl, useServerStore } from '@/stores/serverStore';

/** Which window this webview is: `?view=dictate` and `?view=onboarding` are their own. */
function currentView(): string | null {
  if (typeof window === 'undefined') return null;
  return new URLSearchParams(window.location.search).get('view');
}

/**
 * Validate that a health response has the expected Kass-specific shape.
 * Prevents misidentifying an unrelated service on the same port.
 */
function isKassHealthResponse(health: HealthResponse): boolean {
  return (
    health?.status === 'healthy' &&
    typeof health.model_loaded === 'boolean' &&
    typeof health.gpu_available === 'boolean'
  );
}

/**
 * Check whether a startup error indicates the port is occupied by a local
 * server (which we should try to reuse via health-check polling) vs. a real
 * failure (missing sidecar, signing issue, etc.) that should surface immediately.
 */
function isPortInUseError(error: unknown): boolean {
  const msg = error instanceof Error ? error.message : String(error);
  return (
    msg.includes('already in use') ||
    msg.includes('port') ||
    msg.includes('EADDRINUSE') ||
    msg.includes('address already in use')
  );
}

const LOADING_MESSAGES = [
  'Starting the dictation server...',
  'Loading the speech recognizer...',
  'Warming up Whisper...',
  'Preparing the cleanup model...',
  'Loading your writing style...',
  'Opening the captures library...',
  'Tuning the microphone pipeline...',
  'Almost ready to listen...',
];

function App() {
  useThemeSync();

  // The dictate window runs in a separate Tauri webview that must skip
  // server bootstrap (the main window owns that lifecycle) and render only
  // the floating recording surface. Split into a sibling component so the
  // main app's hooks are not called on the dictate path.
  const view = currentView();
  if (view === 'dictate') {
    return <DictateWindow />;
  }
  // Onboarding runs in its own window once the main window has the server up.
  if (view === 'onboarding') {
    return <OnboardingWindow />;
  }
  return <MainApp />;
}

function MainApp() {
  const platform = usePlatform();
  const [serverReady, setServerReady] = useState(false);
  const [startupError, setStartupError] = useState<string | null>(null);
  const [loadingMessageIndex, setLoadingMessageIndex] = useState(0);
  /** Another Mac's server being reached (beta `network`). */
  const [connectingTo, setConnectingTo] = useState<string | null>(null);
  const serverStartingRef = useRef(false);

  // First run opens the onboarding window, which owns the chord until it's done.
  const onboardingOwnsChord = useOnboardingLauncher(serverReady);
  // Replay the saved chord into the Rust hotkey listener every time
  // capture_settings resolves or the user edits the chord.
  useChordSync({ paused: onboardingOwnsChord });
  // Dictation into Kass's own fields arrives here instead of as a paste.
  useInAppDictationInsert();
  // Rust plays the dictation chimes; it needs their saved settings.
  useSoundCueSync();
  // kass:// links open their screen once the router is showing.
  useDeepLinks(serverReady);
  // Captures dictated on another Mac sharing the server (beta `network`).
  useCaptureEvents(serverReady);

  // Hooks above (the chord's settings among them) ask the server before it
  // is up. Whatever failed for that is asked again once it is, so the global
  // keys work without the window ever being opened.
  useEffect(() => {
    if (!serverReady) return;
    queryClient.refetchQueries({ predicate: (query) => query.state.status === 'error' });
  }, [serverReady]);

  // Setup lifecycle callbacks
  useEffect(() => {
    platform.lifecycle.onServerReady = () => {
      setServerReady(true);
    };
    // Empty dependency array - platform is stable from context, only run once
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform.lifecycle]);

  // Subscribe to server logs
  useEffect(() => {
    const unsubscribe = platform.lifecycle.subscribeToServerLogs((entry) => {
      useLogStore.getState().addEntry(entry);
    });
    return unsubscribe;
  }, [platform.lifecycle]);

  // Auto-start server when running in Tauri (production only)
  useEffect(() => {
    if (!platform.metadata.isTauri) {
      setServerReady(true); // Web assumes server is running
      return;
    }

    // Only auto-start server in production mode
    // In dev mode, user runs server separately
    if (!import.meta.env?.PROD) {
      console.log('Dev mode: Skipping auto-start of server (run it separately)');
      setServerReady(true); // Mark as ready so UI doesn't show loading screen
      // Mark that server was not started by app (so we don't try to stop it on close)
      window.__kassServerStartedByApp = false;
      return;
    }

    // Auto-start server in production
    if (serverStartingRef.current) {
      return;
    }

    serverStartingRef.current = true;

    // Poll the health endpoint until it answers with a valid Kass payload,
    // then show the main UI.
    const pollForServer = (timeoutMessage: string) => {
      let found = false;
      const check = async () => {
        try {
          const health = await apiClient.getHealth();
          if (found) return;
          if (!isKassHealthResponse(health)) {
            console.log('Health response is not from a Kass server, keep polling...');
            return;
          }
          console.log('Kass server detected via health check');
          found = true;
          clearInterval(pollInterval);
          clearTimeout(giveUp);
          setServerReady(true);
        } catch {
          // Server not ready yet, keep polling
        }
      };
      const pollInterval = setInterval(check, 2000);
      void check();

      // Stop polling after 2 minutes and surface the failure
      const giveUp = setTimeout(() => {
        clearInterval(pollInterval);
        serverStartingRef.current = false;
        setStartupError(timeoutMessage);
      }, 120_000);
    };

    const startBundledServer = () => {
      const customModelsDir = useServerStore.getState().customModelsDir;
      console.log('Production mode: Starting bundled server...');

      platform.lifecycle
        .startServer(customModelsDir)
        .then((serverUrl) => {
          console.log('Server is ready at:', serverUrl);
          setServerReady(true);
          // Mark that we started the server (so we know to stop it on close)
          window.__kassServerStartedByApp = true;
        })
        .catch((error) => {
          console.error('Failed to auto-start server:', error);
          serverStartingRef.current = false;
          window.__kassServerStartedByApp = false;

          // Only fall back to health-check polling when the error indicates the
          // port is occupied (likely an external server). For real failures
          // (missing sidecar, signing issues, etc.) surface the error immediately.
          if (!isPortInUseError(error)) {
            const msg = error instanceof Error ? error.message : String(error);
            console.error('Real startup failure — not polling:', msg);
            setStartupError(msg);
            return;
          }

          // A local server may already be running on the port (e.g. started
          // by hand via python/uvicorn).
          console.log('Falling back to health-check polling...');
          pollForServer(
            'Could not connect to a Kass server within 2 minutes. ' +
              'Please check that the server is running and try again.',
          );
        });
    };

    // Beta `network`: with another Mac's server set, use it and start none here.
    void readUpdateChannel().then(() => {
      const remote = remoteServerUrl();
      if (!remote) {
        startBundledServer();
        return;
      }
      console.log('Using the Kass server at', remote);
      window.__kassServerStartedByApp = false;
      setConnectingTo(remote);
      pollForServer(
        `Could not reach Kass at ${remote} within 2 minutes. Check that Kass is open on that ` +
          'Mac with "Share with other Macs" on, or stop using it from Settings.',
      );
    });

    // Window close and app exit stop the server (see the close handler and Rust)
    return () => {
      serverStartingRef.current = false;
    };
    // Empty dependency array - platform is stable from context, only run once
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform.metadata.isTauri, platform.lifecycle]);

  // Cycle through loading messages every 3 seconds
  useEffect(() => {
    if (!platform.metadata.isTauri || serverReady) {
      return;
    }

    const interval = setInterval(() => {
      setLoadingMessageIndex((prev) => (prev + 1) % LOADING_MESSAGES.length);
    }, 3000);

    return () => clearInterval(interval);
  }, [serverReady, platform.metadata.isTauri]);

  // Show loading screen while server is starting in Tauri
  if (platform.metadata.isTauri && !serverReady) {
    return (
      <div
        className={cn(
          'min-h-screen bg-background flex items-center justify-center',
          TOP_SAFE_AREA_PADDING,
        )}
      >
        <TitleBarDragRegion />
        <div className="text-center space-y-6">
          <div className="flex justify-center relative">
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="w-48 h-48 rounded-full bg-accent/20 blur-3xl" />
            </div>
            <img
              src={kassLogo}
              alt="Kass"
              className="w-48 h-48 object-contain animate-fade-in-scale relative z-10"
            />
          </div>
          {startupError ? (
            <div className="animate-fade-in-delayed max-w-md mx-auto space-y-3">
              <p className="text-lg font-medium text-destructive">Server startup failed</p>
              <p className="text-sm text-muted-foreground">{startupError}</p>
              <button
                type="button"
                className="mt-2 px-4 py-2 text-sm rounded-md bg-primary text-primary-foreground hover:bg-primary/90 transition-colors"
                onClick={() => {
                  setStartupError(null);
                  serverStartingRef.current = false;
                  // Trigger a re-mount of the effect by toggling state
                  window.location.reload();
                }}
              >
                Retry
              </button>
              {connectingTo && (
                <button
                  type="button"
                  className="mt-2 ml-2 px-4 py-2 text-sm rounded-md border border-border hover:bg-muted transition-colors"
                  onClick={() => {
                    useServerStore.getState().setRemoteServerUrl(null);
                    window.location.reload();
                  }}
                >
                  Use this Mac instead
                </button>
              )}
            </div>
          ) : (
            <div className="animate-fade-in-delayed">
              <ShinyText
                text={
                  connectingTo
                    ? `Connecting to ${connectingTo}...`
                    : LOADING_MESSAGES[loadingMessageIndex]
                }
                className="text-lg font-medium text-muted-foreground"
                speed={2}
                color="hsl(var(--muted-foreground))"
                shineColor="hsl(var(--foreground))"
              />
            </div>
          )}
        </div>
      </div>
    );
  }

  return <RouterProvider router={router} />;
}

export default App;
