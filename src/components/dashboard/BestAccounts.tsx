import { TrendingUp } from 'lucide-react';
import type { Account } from '../../types/account';
import { useTranslation } from 'react-i18next';

interface BestAccountsProps {
    accounts: Account[];
    currentAccountId?: string;
    onSwitch?: (accountId: string) => void;
}

export default function BestAccounts({ accounts, currentAccountId, onSwitch }: BestAccountsProps) {
    const { t } = useTranslation();

    const geminiSorted = accounts
        .filter(a => a.id !== currentAccountId)
        .map(a => {
            const proQuota = a.quota?.models?.find(m => m.name.toLowerCase() === 'gemini-3-pro-high')?.percentage || 0;
            const flashQuota = a.quota?.models?.find(m => m.name.toLowerCase() === 'gemini-3-flash')?.percentage || 0;
            return { ...a, quotaVal: Math.round(proQuota * 0.7 + flashQuota * 0.3) };
        })
        .filter(a => a.quotaVal > 0)
        .sort((a, b) => b.quotaVal - a.quotaVal);

    const claudeSorted = accounts
        .filter(a => a.id !== currentAccountId)
        .map(a => ({ ...a, quotaVal: a.quota?.models?.find(m => m.name.toLowerCase().includes('claude'))?.percentage || 0 }))
        .filter(a => a.quotaVal > 0)
        .sort((a, b) => b.quotaVal - a.quotaVal);

    let bestGemini = geminiSorted[0];
    let bestClaude = claudeSorted[0];

    if (bestGemini && bestClaude && bestGemini.id === bestClaude.id) {
        const nextGemini = geminiSorted[1];
        const nextClaude = claudeSorted[1];
        const scoreA = bestGemini.quotaVal + (nextClaude?.quotaVal || 0);
        const scoreB = (nextGemini?.quotaVal || 0) + bestClaude.quotaVal;
        if (nextClaude && (!nextGemini || scoreA >= scoreB)) bestClaude = nextClaude;
        else if (nextGemini) bestGemini = nextGemini;
    }

    return (
        <div className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200 h-full flex flex-col">
            <h2 className="text-base font-semibold text-gray-900 dark:text-base-content mb-3 flex items-center gap-2">
                <TrendingUp className="w-4 h-4 text-blue-500 dark:text-blue-400" />
                {t('dashboard.best_accounts', 'Best Accounts')}
            </h2>
            <div className="space-y-2 flex-1">
                {bestGemini && (
                    <div className="flex items-center justify-between p-2.5 bg-green-50 dark:bg-green-900/20 rounded-lg border border-green-100 dark:border-green-900/30">
                        <div className="flex-1 min-w-0">
                            <div className="text-[10px] text-green-600 dark:text-green-400 font-medium mb-0.5">{t('dashboard.for_gemini', 'For Gemini')}</div>
                            <div className="font-medium text-sm text-gray-900 dark:text-base-content truncate">{bestGemini.email}</div>
                        </div>
                        <div className="ml-2 px-2 py-0.5 bg-green-500 text-white text-xs font-semibold rounded-full">{bestGemini.quotaVal}%</div>
                    </div>
                )}
                {bestClaude && (
                    <div className="flex items-center justify-between p-2.5 bg-cyan-50 dark:bg-cyan-900/20 rounded-lg border border-cyan-100 dark:border-cyan-900/30">
                        <div className="flex-1 min-w-0">
                            <div className="text-[10px] text-cyan-600 dark:text-cyan-400 font-medium mb-0.5">{t('dashboard.for_claude', 'For Claude')}</div>
                            <div className="font-medium text-sm text-gray-900 dark:text-base-content truncate">{bestClaude.email}</div>
                        </div>
                        <div className="ml-2 px-2 py-0.5 bg-cyan-500 text-white text-xs font-semibold rounded-full">{bestClaude.quotaVal}%</div>
                    </div>
                )}
                {!bestGemini && !bestClaude && (
                    <div className="text-center py-4 text-gray-400 text-sm">{t('accounts.list.empty', 'No data')}</div>
                )}
            </div>
            {(bestGemini || bestClaude) && onSwitch && (
                <div className="mt-auto pt-3">
                    <button className="w-full px-3 py-1.5 bg-blue-500 text-white text-xs font-medium rounded-lg hover:bg-blue-600 transition-colors"
                        onClick={() => {
                            const targetId = bestClaude && (!bestGemini || bestClaude.quotaVal > bestGemini.quotaVal) ? bestClaude.id : bestGemini?.id;
                            if (targetId) onSwitch(targetId);
                        }}>
                        {t('dashboard.switch_best', 'Switch to Best')}
                    </button>
                </div>
            )}
        </div>
    );
}
