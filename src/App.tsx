import { createBrowserRouter, RouterProvider } from 'react-router-dom';
import { useEffect } from 'react';
import { useConfigStore } from './stores/useConfigStore';
import { useAccountStore } from './stores/useAccountStore';
import { useTranslation } from 'react-i18next';
import { isTauri } from './utils/env';

// Page components
import Dashboard from './pages/Dashboard';
import Accounts from './pages/Accounts';
import ApiProxy from './pages/ApiProxy';
import Monitor from './pages/Monitor';
import TokenStats from './pages/TokenStats';
import UserToken from './pages/UserToken';
import Security from './pages/Security';
import Settings from './pages/Settings';

// Layout & common components
import Layout from './components/layout/Layout';
import ThemeManager from './components/common/ThemeManager';

const router = createBrowserRouter([
    {
        path: '/',
        element: <Layout />,
        children: [
            { index: true, element: <Dashboard /> },
            { path: 'accounts', element: <Accounts /> },
            { path: 'api-proxy', element: <ApiProxy /> },
            { path: 'monitor', element: <Monitor /> },
            { path: 'token-stats', element: <TokenStats /> },
            { path: 'user-token', element: <UserToken /> },
            { path: 'security', element: <Security /> },
            { path: 'settings', element: <Settings /> },
        ],
    },
]);

function App() {
    const { config, loadConfig } = useConfigStore();
    const { fetchCurrentAccount, fetchAccounts } = useAccountStore();
    const { i18n } = useTranslation();

    useEffect(() => {
        loadConfig();
    }, [loadConfig]);

    // Sync language from config
    useEffect(() => {
        if (config?.language) {
            i18n.changeLanguage(config.language);
            document.documentElement.dir = config.language === 'ar' ? 'rtl' : 'ltr';
        }
    }, [config?.language, i18n]);

    // Listen for tray events (Tauri only)
    useEffect(() => {
        if (!isTauri()) return;
        const unlistenPromises: Promise<() => void>[] = [];

        import('@tauri-apps/api/event').then(({ listen }) => {
            unlistenPromises.push(
                listen('tray://account-switched', () => {
                    fetchCurrentAccount();
                    fetchAccounts();
                })
            );
            unlistenPromises.push(
                listen('tray://refresh-current', () => {
                    fetchCurrentAccount();
                    fetchAccounts();
                })
            );
        });

        return () => {
            Promise.all(unlistenPromises).then(unlisteners => {
                unlisteners.forEach(unlisten => unlisten());
            });
        };
    }, [fetchCurrentAccount, fetchAccounts]);

    return (
        <>
            <ThemeManager />
            <RouterProvider router={router} />
        </>
    );
}

export default App;
