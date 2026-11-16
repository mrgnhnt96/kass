import { AudioKeepAlive } from '@/components/AudioPlayer/AudioKeepAlive';
import { TitleBarDragRegion } from '@/components/TitleBarDragRegion';
import { TOP_SAFE_AREA_PADDING } from '@/lib/constants/ui';
import { nativeShell } from '@/lib/nativeShell';
import { cn } from '@/lib/utils/cn';

interface AppFrameProps {
  children: React.ReactNode;
}

export function AppFrame({ children }: AppFrameProps) {
  // In the native shell the title bar is the shell's, above this page.
  return (
    <div
      className={cn(
        'h-screen bg-background flex flex-col overflow-hidden',
        !nativeShell && TOP_SAFE_AREA_PADDING,
      )}
    >
      {!nativeShell && <TitleBarDragRegion />}
      <AudioKeepAlive />
      {children}
    </div>
  );
}
