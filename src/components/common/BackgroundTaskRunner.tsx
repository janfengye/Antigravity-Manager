import { useEffect, useRef } from 'react';
import { useConfigStore } from '../../stores/useConfigStore';
import { useAccountStore } from '../../stores/useAccountStore';

function BackgroundTaskRunner() {
    const { config } = useConfigStore();
    const { refreshAllQuotas } = useAccountStore();

    // Use refs to track previous state to detect "off -> on" transitions
    const prevAutoRefreshRef = useRef(false);

    // Auto Refresh Quota Effect
    useEffect(() => {
        if (!config) return;

        let intervalId: ReturnType<typeof setTimeout> | null = null;
        const { auto_refresh, refresh_interval } = config;

        // Check if we just turned it on
        if (auto_refresh && !prevAutoRefreshRef.current) {
            console.log('[BackgroundTask] Auto-refresh enabled, executing immediately...');
            refreshAllQuotas();
        }
        prevAutoRefreshRef.current = auto_refresh;

        if (auto_refresh && refresh_interval > 0) {
            console.log(`[BackgroundTask] Starting auto-refresh quota timer: ${refresh_interval} mins`);
            intervalId = setInterval(() => {
                console.log('[BackgroundTask] Auto-refreshing all quotas...');
                refreshAllQuotas();
            }, Math.min(refresh_interval * 60 * 1000, 2147483647));
        }

        return () => {
            if (intervalId) {
                console.log('[BackgroundTask] Clearing auto-refresh timer');
                clearInterval(intervalId);
            }
        };
    }, [config?.auto_refresh, config?.refresh_interval]);

    // Render nothing
    return null;
}

export default BackgroundTaskRunner;
