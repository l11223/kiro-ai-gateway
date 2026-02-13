import { useEffect, useRef } from 'react';
import { useConfigStore } from '../../stores/useConfigStore';
import { useAccountStore } from '../../stores/useAccountStore';

export default function BackgroundTaskRunner() {
    const { config } = useConfigStore();
    const { refreshAllQuotas } = useAccountStore();
    const prevAutoRefreshRef = useRef(false);

    useEffect(() => {
        if (!config) return;

        let intervalId: ReturnType<typeof setTimeout> | null = null;
        const { auto_refresh, refresh_interval } = config;

        if (auto_refresh && !prevAutoRefreshRef.current) {
            refreshAllQuotas();
        }
        prevAutoRefreshRef.current = auto_refresh;

        if (auto_refresh && refresh_interval > 0) {
            intervalId = setInterval(() => {
                refreshAllQuotas();
            }, refresh_interval * 60 * 1000);
        }

        return () => {
            if (intervalId) clearInterval(intervalId);
        };
    }, [config?.auto_refresh, config?.refresh_interval, refreshAllQuotas]);

    return null;
}
