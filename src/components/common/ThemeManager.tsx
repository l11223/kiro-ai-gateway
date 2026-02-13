import { useEffect } from 'react';
import { useConfigStore } from '../../stores/useConfigStore';
import { isTauri, isLinux } from '../../utils/env';

export default function ThemeManager() {
    const { config, loadConfig } = useConfigStore();

    useEffect(() => {
        const init = async () => {
            await loadConfig();
            setTimeout(async () => {
                if (isTauri()) {
                    const { getCurrentWindow } = await import('@tauri-apps/api/window');
                    await getCurrentWindow().show();
                }
            }, 100);
        };
        init();
    }, [loadConfig]);

    useEffect(() => {
        if (!config) return;

        const applyTheme = async (theme: string) => {
            const root = document.documentElement;
            const isDark = theme === 'dark';

            try {
                if (!isLinux() && isTauri()) {
                    const { getCurrentWindow } = await import('@tauri-apps/api/window');
                    const bgColor = isDark ? '#1d232a' : '#FAFBFC';
                    getCurrentWindow().setBackgroundColor(bgColor).catch(() => {});
                    const { invoke } = await import('@tauri-apps/api/core');
                    invoke('set_window_theme', { theme }).catch(() => {});
                }
            } catch { /* ignore */ }

            root.setAttribute('data-theme', theme);
            root.style.backgroundColor = isDark ? '#1d232a' : '#FAFBFC';
            if (isDark) root.classList.add('dark');
            else root.classList.remove('dark');
        };

        const theme = config.theme || 'light';
        localStorage.setItem('app-theme-preference', theme);

        if (theme === 'system') {
            const mq = window.matchMedia('(prefers-color-scheme: dark)');
            const handler = (e: MediaQueryListEvent | MediaQueryList) => applyTheme(e.matches ? 'dark' : 'light');
            handler(mq);
            mq.addEventListener('change', handler);
            return () => mq.removeEventListener('change', handler);
        } else {
            applyTheme(theme);
        }
    }, [config?.theme]);

    return null;
}
