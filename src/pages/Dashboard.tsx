import { AlertTriangle, ArrowRight, Bot, Download, RefreshCw, Sparkles, Users } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { useAccountStore } from '../stores/useAccountStore';
import { exportAccounts } from '../services/accountService';
import { cn } from '../utils/cn';

function Dashboard() {
    const { t } = useTranslation();
    const navigate = useNavigate();
    const { accounts, currentAccount, fetchAccounts, fetchCurrentAccount, switchAccount, refreshQuota } = useAccountStore();

    const [isRefreshing, setIsRefreshing] = useState(false);

    useEffect(() => { fetchAccounts(); fetchCurrentAccount(); }, []);

    const stats = useMemo(() => {
        const getAvg = (modelName: string) => {
            const quotas = accounts.map(a => a.quota?.models.find(m => m.name.toLowerCase() === modelName)?.percentage || 0).filter(q => q > 0);
            return quotas.length > 0 ? Math.round(quotas.reduce((a, b) => a + b, 0) / quotas.length) : 0;
        };
        const lowQuotaCount = accounts.filter(a => {
            if (a.quota?.is_forbidden) return false;
            const g = a.quota?.models.find(m => m.name.toLowerCase().includes('gemini'))?.percentage || 0;
            const c = a.quota?.models.find(m => m.name.toLowerCase().includes('claude'))?.percentage || 0;
            return g < 20 || c < 20;
        }).length;
        return {
            total: accounts.length,
            avgGemini: getAvg('gemini-3-pro-high'),
            avgGeminiImage: getAvg('gemini-3-pro-image'),
            avgClaude: getAvg('claude-sonnet-4-5'),
            lowQuota: lowQuotaCount,
        };
    }, [accounts]);

    const handleRefreshCurrent = async () => {
        if (!currentAccount) return;
        setIsRefreshing(true);
        try { await refreshQuota(currentAccount.id); await fetchCurrentAccount(); } catch { /* handled */ }
        finally { setIsRefreshing(false); }
    };

    const bestAccounts = useMemo(() => {
        return [...accounts]
            .filter(a => !a.disabled && !a.proxy_disabled && !a.quota?.is_forbidden)
            .sort((a, b) => {
                const aMax = Math.max(...(a.quota?.models.map(m => m.percentage) || [0]));
                const bMax = Math.max(...(b.quota?.models.map(m => m.percentage) || [0]));
                return bMax - aMax;
            })
            .slice(0, 5);
    }, [accounts]);

    const handleExport = async () => {
        try {
            const ids = accounts.map(a => a.id);
            const response = await exportAccounts(ids);
            if (!response.accounts?.length) return;
            const content = JSON.stringify(response.accounts, null, 2);
            const fileName = `kiro_accounts_${new Date().toISOString().split('T')[0]}.json`;
            const blob = new Blob([content], { type: 'application/json' });
            const url = URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url; a.download = fileName;
            document.body.appendChild(a); a.click();
            document.body.removeChild(a);
            URL.revokeObjectURL(url);
        } catch (error) { console.error('Export failed:', error); }
    };

    const statCards = [
        { icon: Users, color: 'blue', value: stats.total, label: t('dashboard.total_accounts', 'Total Accounts') },
        { icon: Sparkles, color: 'green', value: `${stats.avgGemini}%`, label: t('dashboard.avg_gemini', 'Avg Gemini Pro') },
        { icon: Sparkles, color: 'purple', value: `${stats.avgGeminiImage}%`, label: t('dashboard.avg_gemini_image', 'Avg Gemini Image') },
        { icon: Bot, color: 'cyan', value: `${stats.avgClaude}%`, label: t('dashboard.avg_claude', 'Avg Claude') },
        { icon: AlertTriangle, color: 'orange', value: stats.lowQuota, label: t('dashboard.low_quota_accounts', 'Low Quota') },
    ];

    return (
        <div className="h-full w-full overflow-y-auto">
            <div className="p-5 space-y-4 max-w-7xl mx-auto">
                {/* Header */}
                <div className="flex justify-between items-center">
                    <h1 className="text-2xl font-bold text-gray-900 dark:text-base-content">
                        {currentAccount ? `👋 ${currentAccount.name || currentAccount.email.split('@')[0]}` : t('dashboard.title')}
                    </h1>
                    <div className="flex gap-2">
                        <button className={cn('px-3 py-1.5 bg-blue-500 text-white text-xs font-medium rounded-lg hover:bg-blue-600 flex items-center gap-1.5 shadow-sm', (isRefreshing || !currentAccount) && 'opacity-70 cursor-not-allowed')} onClick={handleRefreshCurrent} disabled={isRefreshing || !currentAccount}>
                            <RefreshCw className={cn('w-3.5 h-3.5', isRefreshing && 'animate-spin')} />
                            <span className="hidden sm:inline">{isRefreshing ? t('common.refreshing') : t('common.refresh')}</span>
                        </button>
                    </div>
                </div>

                {/* Stats */}
                <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
                    {statCards.map((card, i) => (
                        <div key={i} className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200">
                            <div className="flex items-center justify-between mb-2">
                                <div className={`p-1.5 bg-${card.color}-50 dark:bg-${card.color}-900/20 rounded-md`}>
                                    <card.icon className={`w-4 h-4 text-${card.color}-500`} />
                                </div>
                            </div>
                            <div className="text-2xl font-bold text-gray-900 dark:text-base-content mb-0.5">{card.value}</div>
                            <div className="text-xs text-gray-500 dark:text-gray-400">{card.label}</div>
                        </div>
                    ))}
                </div>

                {/* Current Account & Best Accounts */}
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                    {/* Current Account */}
                    <div className="bg-white dark:bg-base-100 rounded-xl p-5 shadow-sm border border-gray-100 dark:border-base-200">
                        <h2 className="text-lg font-semibold text-gray-900 dark:text-base-content mb-4">{t('dashboard.current_account', 'Current Account')}</h2>
                        {currentAccount ? (
                            <div>
                                <div className="flex items-center gap-3 mb-3">
                                    <div className="w-10 h-10 rounded-full bg-gradient-to-br from-blue-400 to-purple-500 flex items-center justify-center text-white font-bold">{currentAccount.email[0].toUpperCase()}</div>
                                    <div>
                                        <div className="font-medium text-gray-900 dark:text-base-content">{currentAccount.email}</div>
                                        <div className="text-xs text-gray-500">{currentAccount.quota?.subscription_tier || 'Unknown'}</div>
                                    </div>
                                </div>
                                <div className="space-y-2">
                                    {currentAccount.quota?.models.slice(0, 5).map(q => (
                                        <div key={q.name} className="flex items-center gap-2">
                                            <span className="text-xs text-gray-500 w-32 truncate">{q.name}</span>
                                            <div className="flex-1 h-2 bg-gray-100 dark:bg-base-200 rounded-full overflow-hidden">
                                                <div className={cn('h-full rounded-full', q.percentage >= 50 ? 'bg-green-500' : q.percentage >= 20 ? 'bg-yellow-500' : 'bg-red-500')} style={{ width: `${q.percentage}%` }} />
                                            </div>
                                            <span className="text-xs font-mono w-10 text-right">{q.percentage}%</span>
                                        </div>
                                    ))}
                                </div>
                            </div>
                        ) : (
                            <div className="text-center py-8 text-gray-400">{t('accounts.list.empty')}</div>
                        )}
                    </div>

                    {/* Best Accounts */}
                    <div className="bg-white dark:bg-base-100 rounded-xl p-5 shadow-sm border border-gray-100 dark:border-base-200">
                        <h2 className="text-lg font-semibold text-gray-900 dark:text-base-content mb-4">{t('dashboard.best_accounts', 'Best Accounts')}</h2>
                        <div className="space-y-3">
                            {bestAccounts.map((account, i) => {
                                const maxQuota = Math.max(...(account.quota?.models.map(m => m.percentage) || [0]));
                                return (
                                    <div key={account.id} className="flex items-center gap-3 p-2 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 transition-colors cursor-pointer" onClick={() => switchAccount(account.id)}>
                                        <span className="text-xs font-bold text-gray-400 w-5">#{i + 1}</span>
                                        <div className="w-7 h-7 rounded-full bg-gradient-to-br from-blue-400 to-purple-500 flex items-center justify-center text-white text-[10px] font-bold">{account.email[0].toUpperCase()}</div>
                                        <div className="flex-1 min-w-0">
                                            <div className="text-sm text-gray-900 dark:text-base-content truncate">{account.email}</div>
                                        </div>
                                        <span className={cn('text-xs font-mono font-medium', maxQuota >= 50 ? 'text-green-600' : maxQuota >= 20 ? 'text-yellow-600' : 'text-red-600')}>{maxQuota}%</span>
                                    </div>
                                );
                            })}
                            {bestAccounts.length === 0 && <div className="text-center py-4 text-gray-400 text-sm">{t('accounts.list.empty')}</div>}
                        </div>
                    </div>
                </div>

                {/* Quick links */}
                <div className="grid grid-cols-2 gap-3">
                    <button className="bg-indigo-50 dark:bg-indigo-900/20 rounded-lg p-3 shadow-sm border border-indigo-100 dark:border-indigo-900/30 hover:shadow-md transition-all flex items-center justify-between group" onClick={() => navigate('/accounts')}>
                        <span className="text-indigo-700 dark:text-indigo-300 font-medium text-sm">{t('dashboard.view_all_accounts', 'View All Accounts')}</span>
                        <ArrowRight className="w-4 h-4 text-indigo-400 group-hover:translate-x-1 transition-all" />
                    </button>
                    <button className="bg-purple-50 dark:bg-purple-900/20 rounded-lg p-3 shadow-sm border border-purple-100 dark:border-purple-900/30 hover:shadow-md transition-all flex items-center justify-between group" onClick={handleExport}>
                        <span className="text-purple-700 dark:text-purple-300 font-medium text-sm">{t('dashboard.export_data', 'Export Data')}</span>
                        <Download className="w-4 h-4 text-purple-400 transition-all" />
                    </button>
                </div>
            </div>
        </div>
    );
}

export default Dashboard;
