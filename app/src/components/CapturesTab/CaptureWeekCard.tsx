import { keepPreviousData, useQuery } from '@tanstack/react-query';
import { Link } from '@tanstack/react-router';
import { ArrowRight } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { ChartTip } from '@/components/InsightsTab/ChartTip';
import {
  formatCount,
  formatDay,
  formatSaved,
  formatWeekday,
  parseLocal,
  statsKey,
} from '@/components/InsightsTab/usageFormat';
import { Button } from '@/components/ui/button';
import { apiClient } from '@/lib/api/client';
import type { CaptureAppFilter, UsagePoint } from '@/lib/api/types';
import { cn } from '@/lib/utils/cn';
import { useUIStore } from '@/stores/uiStore';

function Stat({ value, label }: { value: string; label: string }) {
  return (
    <div className="shrink-0 whitespace-nowrap">
      <b className="block text-xl font-bold leading-tight tabular-nums">{value}</b>
      <span className="text-[11px] text-muted-foreground">{label}</span>
    </div>
  );
}

/** Words per day as seven small bars, today's brightest, each with its words on hover. */
function WeekBars({ days, label }: { days: UsagePoint[]; label: string }) {
  const { t } = useTranslation();
  const max = Math.max(1, ...days.map((d) => d.words ?? 0)) * 1.05;
  return (
    <div role="img" aria-label={label} className="ml-auto shrink-0 flex flex-col gap-1">
      <div className="relative h-[30px] flex items-end gap-1">
        {days.map((day, i) => {
          const words = day.words ?? 0;
          const today = i === days.length - 1;
          return (
            <div key={day.start} className="group/bar relative h-full w-3.5 flex items-end">
              <i
                className={cn(
                  'block w-full min-h-0.5 rounded-t-[3px] transition-colors group-hover/bar:bg-accent/70',
                  today ? 'bg-accent' : 'bg-accent/45',
                )}
                style={{ height: `${(words / max) * 100}%` }}
              />
              <ChartTip
                align="end"
                className="hidden group-hover/bar:block bottom-[calc(100%+6px)] left-[calc(100%+6px)]"
              >
                <b className="block text-xs tabular-nums">
                  {words
                    ? t('captures.week.words', { count: words, formatted: formatCount(words) })
                    : t('captures.week.noDictation')}
                </b>
                <span className="text-muted-foreground">
                  {formatDay(parseLocal(day.start))}
                  {today && ` · ${t('captures.week.today')}`}
                </span>
              </ChartTip>
            </div>
          );
        })}
      </div>
      <div aria-hidden className="flex gap-1">
        {days.map((day) => (
          <span key={day.start} className="w-3.5 text-center text-[9.5px] text-muted-foreground">
            {formatWeekday(parseLocal(day.start)).charAt(0)}
          </span>
        ))}
      </div>
    </div>
  );
}

/**
 * The "Last 7 days" card above the capture list: words, speaking pace and
 * time saved, and words per day, for the app the list shows. The arrow
 * opens Insights on the same app.
 */
export function CaptureWeekCard({
  filter,
  appName,
}: {
  filter: CaptureAppFilter;
  /** The app the list is filtered to, if any. */
  appName?: string;
}) {
  const { t } = useTranslation();
  const setInsightsPeriod = useUIStore((s) => s.setInsightsPeriod);
  const { data: stats } = useQuery({
    queryKey: statsKey('7d', filter),
    queryFn: () => apiClient.getCaptureStats('7d', filter),
    placeholderData: keepPreviousData,
  });
  const totals = stats?.current;
  const label = appName ? t('captures.week.labelApp', { app: appName }) : t('captures.week.label');

  return (
    <section
      aria-label={label}
      className="flex flex-col gap-1 rounded-lg border border-border bg-card px-3.5 pt-2 pb-3"
    >
      <div className="h-6 flex items-center justify-between">
        <h2 className="min-w-0 truncate font-mono text-[11px] uppercase tracking-wider text-muted-foreground">
          {label}
        </h2>
        <Button
          variant="ghost"
          size="icon"
          className="size-6 -mr-1.5 shrink-0 text-accent hover:text-accent [&_svg]:size-[15px]"
          asChild
        >
          <Link
            to="/insights"
            search={{
              period: '7d',
              ...(filter.kind === 'app' ? { app: filter.bundleId } : { apps: filter.kind }),
            }}
            aria-label={t('captures.week.open')}
            title={t('captures.week.open')}
            onClick={() => setInsightsPeriod('7d')}
          >
            <ArrowRight strokeWidth={1.7} />
          </Link>
        </Button>
      </div>
      <div className="flex items-end gap-4">
        <Stat
          value={totals ? formatCount(totals.words) : '—'}
          label={t('captures.week.wordsLabel')}
        />
        <Stat
          value={totals?.pace_wpm != null ? String(totals.pace_wpm) : '—'}
          label={t('captures.week.wpm')}
        />
        <Stat
          value={totals ? formatSaved(totals.time_saved_ms) : '—'}
          label={t('captures.week.saved')}
        />
        {stats && (
          <WeekBars
            days={stats.series}
            label={t('captures.week.chart', { app: appName ?? t('captures.apps.all') })}
          />
        )}
      </div>
    </section>
  );
}
