import { Link, Outlet, useLocation, useMatchRoute } from '@tanstack/react-router';
import { useTranslation } from 'react-i18next';
import { isNativePage, nativeShell } from '@/lib/nativeShell';
import { cn } from '@/lib/utils/cn';
import { usePlatform } from '@/platform/PlatformContext';

type SettingsPath =
  | '/settings'
  | '/settings/dictation'
  | '/settings/command-mode'
  | '/settings/transcription'
  | '/settings/writing-style'
  | '/settings/dictionary'
  | '/settings/features'
  | '/settings/logs';

const tabs: Array<{ labelKey: string; path: SettingsPath; tauriOnly?: boolean }> = [
  { labelKey: 'settings.tabs.general', path: '/settings' },
  { labelKey: 'settings.tabs.dictation', path: '/settings/dictation' },
  { labelKey: 'settings.tabs.commandMode', path: '/settings/command-mode' },
  { labelKey: 'settings.tabs.transcription', path: '/settings/transcription' },
  { labelKey: 'settings.tabs.writingStyle', path: '/settings/writing-style' },
  { labelKey: 'settings.tabs.dictionary', path: '/settings/dictionary' },
  { labelKey: 'settings.tabs.features', path: '/settings/features' },
  { labelKey: 'settings.tabs.logs', path: '/settings/logs', tauriOnly: true },
];

/** Settings: a list of sections on the left, the chosen page on the right. */
export function SettingsLayout() {
  const { t } = useTranslation();
  const platform = usePlatform();
  const matchRoute = useMatchRoute();
  const { pathname } = useLocation();
  const fullBleed = Boolean(matchRoute({ to: '/settings/logs' }));
  // The dictionary is two panes side by side, so it gets a wider column.
  const wide = Boolean(matchRoute({ to: '/settings/dictionary' }));

  // The native shell draws the list and its own pages; this shows the rest.
  if (nativeShell && isNativePage(pathname)) return null;

  return (
    <div className="flex h-full min-h-0">
      {!nativeShell && (
        <nav
          aria-label={t('nav.settings')}
          className="w-[220px] shrink-0 flex flex-col gap-0.5 px-2.5 py-5 border-r border-border bg-sidebar"
        >
          <h1 className="px-2.5 pb-3.5 text-lg font-semibold">{t('nav.settings')}</h1>
          {tabs.map((tab) => {
            if (tab.tauriOnly && !platform.metadata.isTauri) return null;
            const isActive = matchRoute({ to: tab.path, fuzzy: false });
            return (
              <Link
                key={tab.path}
                to={tab.path}
                className={cn(
                  'flex h-[34px] items-center rounded-md px-2.5 text-[13px] transition-colors',
                  isActive
                    ? 'bg-muted text-foreground shadow-[inset_2px_0_0_hsl(var(--accent))]'
                    : 'text-muted-foreground hover:text-foreground',
                )}
              >
                {t(tab.labelKey)}
              </Link>
            );
          })}
        </nav>
      )}

      {/* Logs is a full-height pane; the other pages are a readable column. */}
      {fullBleed ? (
        <div className="flex-1 min-w-0 overflow-hidden">
          <Outlet />
        </div>
      ) : (
        <div className="flex-1 min-w-0 overflow-y-auto">
          <div className={cn('px-12 py-7', wide ? 'max-w-[1040px]' : 'max-w-[760px]')}>
            <Outlet />
          </div>
        </div>
      )}
    </div>
  );
}
