import { Loader2, RefreshCw, RotateCw } from 'lucide-react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { useToast } from '@/components/ui/use-toast';
import { useUpdateCheck } from '@/lib/hooks/useUpdateCheck';
import { usePlatform } from '@/platform/PlatformContext';
import { SettingRow } from './SettingRow';

/**
 * The installed version, a check for a newer one, and a restart into it
 * once it has downloaded.
 */
export function VersionRow() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const isTauri = usePlatform().metadata.isTauri;
  const { version, status, checking, check, restarting, restart } = useUpdateCheck();
  // Set once a check the user asked for finds nothing newer.
  const [upToDate, setUpToDate] = useState(false);

  const onCheck = async () => {
    setUpToDate(false);
    try {
      setUpToDate((await check()).state === 'current');
    } catch (error) {
      toast({
        title: t('settings.general.version.checkFailed'),
        description: String(error),
        variant: 'destructive',
      });
    }
  };

  const action =
    status.state === 'ready' ? (
      <Button variant="outline" size="sm" disabled={restarting} onClick={restart}>
        {restarting ? (
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
        ) : (
          <RotateCw className="h-3.5 w-3.5" />
        )}
        {restarting
          ? t('settings.general.version.restarting')
          : t('settings.general.version.restart')}
      </Button>
    ) : (
      isTauri && (
        <Button
          variant="outline"
          size="sm"
          disabled={checking || status.state === 'downloading'}
          onClick={onCheck}
        >
          {checking ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <RefreshCw className="h-3.5 w-3.5" />
          )}
          {checking ? t('settings.general.version.checking') : t('settings.general.version.check')}
        </Button>
      )
    );

  return (
    <SettingRow
      title={t('settings.general.version.title')}
      description={
        status.state === 'ready'
          ? t('settings.general.version.ready', { current: version, latest: status.version })
          : status.state === 'downloading'
            ? t('settings.general.version.downloading', {
                current: version,
                latest: status.version,
              })
            : upToDate
              ? t('settings.general.version.upToDate', { version })
              : t('settings.general.version.current', { version })
      }
      action={action}
    />
  );
}
