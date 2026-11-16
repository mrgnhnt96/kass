import {
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  redirect,
} from '@tanstack/react-router';
import { AppFrame } from '@/components/AppFrame/AppFrame';
import { CapturesTab } from '@/components/CapturesTab/CapturesTab';
import { InsightsTab } from '@/components/InsightsTab/InsightsTab';
import { ModelsTab } from '@/components/ModelsTab/ModelsTab';
import { SetupEntry } from '@/components/Onboarding/SetupEntry';
import { ReportsTab } from '@/components/ReportsTab/ReportsTab';
import { GeneralPage } from '@/components/ServerTab/GeneralPage';
import { LogsPage } from '@/components/ServerTab/LogsPage';
import { SettingsLayout } from '@/components/ServerTab/ServerTab';
import { CommandModePage } from '@/components/Settings/CommandModePage';
import { DictationSettingsPage } from '@/components/Settings/DictationSettingsPage';
import { DictionaryPage } from '@/components/Settings/DictionaryPage';
import { FeaturesPage } from '@/components/Settings/FeaturesPage';
import { TranscriptionSettingsPage } from '@/components/Settings/TranscriptionSettingsPage';
import { WritingStylePage } from '@/components/Settings/WritingStylePage';
import { useFirstRunRedirect } from '@/components/Setup/useFirstRunRedirect';
import { Sidebar } from '@/components/Sidebar';
import { StatusBar } from '@/components/StatusBar';
import { Toaster } from '@/components/ui/toaster';
import { useModelDownloadToast } from '@/lib/hooks/useModelDownloadToast';
import { MODEL_DISPLAY_NAMES, useRestoreActiveTasks } from '@/lib/hooks/useRestoreActiveTasks';
import { nativeShell, useNativeShell } from '@/lib/nativeShell';

// Root layout component
function RootLayout() {
  // Monitor active model downloads and show toasts for them
  const activeDownloads = useRestoreActiveTasks();
  // Send a brand-new user to /setup, once per launch.
  useFirstRunRedirect();
  useNativeShell();

  return (
    <AppFrame>
      {/* In the native shell, the sidebar and status bar are the shell's. */}
      {nativeShell ? (
        <main className="flex-1 min-h-0 min-w-0 overflow-hidden flex flex-col">
          <Outlet />
        </main>
      ) : (
        <>
          <div className="flex flex-1 min-h-0 overflow-hidden border-t border-border">
            <Sidebar />
            <main className="flex-1 min-w-0 overflow-hidden flex flex-col">
              <Outlet />
            </main>
          </div>
          <StatusBar />
        </>
      )}
      {/* The native shell asks about retention and the app's location.
          Download toasts stay here for the screens still in the web view,
          which covers the native ones. */}
      {activeDownloads.map((download) => {
        const displayName = MODEL_DISPLAY_NAMES[download.model_name] || download.model_name;
        return (
          <DownloadToastRestorer
            key={download.model_name}
            modelName={download.model_name}
            displayName={displayName}
          />
        );
      })}

      <Toaster />
    </AppFrame>
  );
}

/**
 * Component that restores a download toast for a specific model.
 */
function DownloadToastRestorer({
  modelName,
  displayName,
}: {
  modelName: string;
  displayName: string;
}) {
  // Use the download toast hook to restore the toast
  useModelDownloadToast({
    modelName,
    displayName,
    enabled: true,
  });

  return null;
}

// Root route with layout
const rootRoute = createRootRoute({
  component: RootLayout,
});

// Index route — dictation is the whole app, so land on Captures.
const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  beforeLoad: () => {
    throw redirect({ to: '/captures' });
  },
});

// Captures route
const capturesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/captures',
  // The native shell draws Captures itself (crates/kass-captures).
  component: nativeShell ? () => null : CapturesTab,
  // `?capture=<id>` opens that capture (the command palette links here).
  validateSearch: (search: Record<string, unknown>): { capture?: string } => ({
    capture: typeof search.capture === 'string' ? search.capture : undefined,
  }),
});

// Reports route: a zip of logs and crash reports to send (desktop only).
const reportsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/reports',
  component: ReportsTab,
});

type InsightsSearch = { app?: string; apps?: 'all' | 'unknown'; period?: string };

// Insights route: usage stats, for all apps or the one picked in Captures.
// The native shell draws Insights itself (crates/kass-insights).
const insightsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/insights',
  component: nativeShell ? () => null : InsightsTab,
  // What the native page opens on (kass-insights link.rs); this page reads
  // the shared app filter and period instead.
  validateSearch: (search: Record<string, unknown>): InsightsSearch => ({
    app: typeof search.app === 'string' ? search.app : undefined,
    apps: search.apps === 'all' || search.apps === 'unknown' ? search.apps : undefined,
    period: typeof search.period === 'string' ? search.period : undefined,
  }),
});

// Models route
const modelsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/models',
  component: ModelsTab,
  // `?model=<name>` opens that model (Settings links here to change one).
  validateSearch: (search: Record<string, unknown>): { model?: string } => ({
    model: typeof search.model === 'string' ? search.model : undefined,
  }),
});

// Settings layout route (parent for sub-tabs)
const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings',
  component: SettingsLayout,
});

// Settings sub-routes
const settingsGeneralRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/',
  component: GeneralPage,
});

const settingsDictationRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/dictation',
  component: DictationSettingsPage,
});

const settingsCommandModeRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/command-mode',
  component: CommandModePage,
});

const settingsTranscriptionRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/transcription',
  component: TranscriptionSettingsPage,
});

const settingsWritingStyleRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/writing-style',
  component: WritingStylePage,
  // `?style=<id>` selects that style (a capture's settings button links here).
  validateSearch: (search: Record<string, unknown>): { style?: string } => ({
    style: typeof search.style === 'string' ? search.style : undefined,
  }),
});

const settingsFeaturesRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/features',
  component: FeaturesPage,
});

const settingsDictionaryRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/dictionary',
  component: DictionaryPage,
  // `?scope=` is `style:<id>` or `app:<bundle id>`; none is everywhere.
  // `?kind=phrases` shows phrases instead of words.
  validateSearch: (search: Record<string, unknown>): { scope?: string; kind?: 'phrases' } => ({
    scope: typeof search.scope === 'string' ? search.scope : undefined,
    kind: search.kind === 'phrases' ? 'phrases' : undefined,
  }),
});

// Dictation, transcription and writing style used to share one page.
const settingsCapturesRedirectRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/captures',
  beforeLoad: () => {
    throw redirect({ to: '/settings/dictation' });
  },
});

const settingsLogsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/logs',
  component: LogsPage,
});

// First-run setup: the onboarding window on desktop, the setup page on the web.
const setupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/setup',
  component: SetupEntry,
});

// Redirect old /server path to /settings
const serverRedirectRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/server',
  beforeLoad: () => {
    throw redirect({ to: '/settings' });
  },
});

// Route tree
const routeTree = rootRoute.addChildren([
  indexRoute,
  capturesRoute,
  insightsRoute,
  modelsRoute,
  reportsRoute,
  settingsRoute.addChildren([
    settingsGeneralRoute,
    settingsDictationRoute,
    settingsCommandModeRoute,
    settingsTranscriptionRoute,
    settingsWritingStyleRoute,
    settingsDictionaryRoute,
    settingsFeaturesRoute,
    settingsCapturesRedirectRoute,
    settingsLogsRoute,
  ]),
  setupRoute,
  serverRedirectRoute,
]);

// Create router
export const router = createRouter({ routeTree });

// Register router for type safety
declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}
