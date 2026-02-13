import { CheckCircle, Mail, Diamond, Gem, Circle, Tag, Lock } from 'lucide-react';
import type { Account } from '../../types/account';
import { formatTimeRemaining } from '../../utils/format';
import { useTranslation } from 'react-i18next';

interface CurrentAccountProps {
    account: Account | null;
    onSwitch?: () => void;
}

export default function CurrentAccount({ account, onSwitch }: CurrentAccountProps) {
    const { t } = useTranslation();

    if (!account) {
        return (
            <div className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200">
                <h2 className="text-base font-semibold text-gray-900 dark:text-base-content mb-2 flex items-center gap-2">
                    <CheckCircle className="w-4 h-4 text-green-500" />
                    {t('dashboard.current_account', 'Current Account')}
                </h2>
                <div className="text-center py-4 text-gray-400 dark:text-gray-500 text-sm">
                    {t('dashboard.no_active_account', 'No active account')}
                </div>
            </div>
        );
    }

    const geminiProModel = account.quota?.models
        ?.filter(m => m.name.toLowerCase() === 'gemini-3-pro-high' || m.name.toLowerCase() === 'gemini-3-pro-low')
        .sort((a, b) => a.percentage - b.percentage)[0];
    const geminiFlashModel = account.quota?.models?.find(m => m.name.toLowerCase() === 'gemini-3-flash');
    const claudeModel = account.quota?.models
        ?.filter(m => ['claude-opus-4-6-thinking', 'claude'].includes(m.name.toLowerCase()))
        .sort((a, b) => a.percentage - b.percentage)[0];

    const renderQuotaBar = (model: { name: string; percentage: number; reset_time: string } | undefined, label: string, protectedKey?: string) => {
        if (!model) return null;
        const p = model.percentage;
        const color = p >= 50 ? 'emerald' : p >= 20 ? 'amber' : 'rose';
        return (
            <div className="space-y-1.5">
                <div className="flex justify-between items-baseline">
                    <span className="text-xs font-medium text-gray-600 dark:text-gray-400 flex items-center gap-1">
                        {protectedKey && account.protected_models?.includes(protectedKey) && <Lock className="w-2.5 h-2.5 text-rose-500" />}
                        {label}
                    </span>
                    <div className="flex items-center gap-2">
                        <span className="text-[10px] text-gray-400 dark:text-gray-500">
                            {model.reset_time ? `R: ${formatTimeRemaining(model.reset_time)}` : t('common.unknown')}
                        </span>
                        <span className={`text-xs font-bold text-${color}-600 dark:text-${color}-400`}>{p}%</span>
                    </div>
                </div>
                <div className="w-full bg-gray-100 dark:bg-base-300 rounded-full h-1.5 overflow-hidden">
                    <div className={`h-full rounded-full transition-all duration-700 bg-gradient-to-r from-${color}-400 to-${color}-500`} style={{ width: `${p}%` }} />
                </div>
            </div>
        );
    };

    const tier = account.quota?.subscription_tier?.toLowerCase() || '';
    const tierBadge = tier.includes('ultra')
        ? <span className="flex items-center gap-1 px-2 py-0.5 rounded-md bg-gradient-to-r from-purple-600 to-pink-600 text-white text-[10px] font-bold shadow-sm shrink-0"><Gem className="w-2.5 h-2.5 fill-current" />ULTRA</span>
        : tier.includes('pro')
            ? <span className="flex items-center gap-1 px-2 py-0.5 rounded-md bg-gradient-to-r from-blue-600 to-indigo-600 text-white text-[10px] font-bold shadow-sm shrink-0"><Diamond className="w-2.5 h-2.5 fill-current" />PRO</span>
            : tier
                ? <span className="flex items-center gap-1 px-2 py-0.5 rounded-md bg-gray-100 dark:bg-white/10 text-gray-500 dark:text-gray-400 text-[10px] font-bold shadow-sm border border-gray-200 dark:border-white/10 shrink-0"><Circle className="w-2.5 h-2.5" />FREE</span>
                : null;

    return (
        <div className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200 h-full flex flex-col">
            <h2 className="text-base font-semibold text-gray-900 dark:text-base-content mb-3 flex items-center gap-2">
                <CheckCircle className="w-4 h-4 text-green-500" />
                {t('dashboard.current_account', 'Current Account')}
            </h2>
            <div className="space-y-4 flex-1">
                <div className="flex items-center gap-3 mb-1">
                    <div className="flex items-center gap-2 flex-1 min-w-0">
                        <Mail className="w-3.5 h-3.5 text-gray-400" />
                        <span className="text-sm font-medium text-gray-700 dark:text-gray-300 truncate">{account.email}</span>
                    </div>
                    {tierBadge}
                    {account.custom_label && (
                        <span className="flex items-center gap-1 px-2 py-0.5 rounded-md bg-orange-100 dark:bg-orange-900/30 text-orange-600 dark:text-orange-400 text-[10px] font-bold shadow-sm shrink-0">
                            <Tag className="w-2.5 h-2.5" />{account.custom_label}
                        </span>
                    )}
                </div>
                {renderQuotaBar(geminiProModel, 'Gemini 3 Pro', 'gemini-3-pro-high')}
                {renderQuotaBar(geminiFlashModel, 'Gemini 3 Flash', 'gemini-3-flash')}
                {renderQuotaBar(claudeModel, 'Claude Series', 'claude')}
            </div>
            {onSwitch && (
                <div className="mt-auto pt-3">
                    <button className="w-full px-3 py-1.5 text-xs text-gray-700 dark:text-gray-300 border border-gray-200 dark:border-base-300 rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 transition-colors"
                        onClick={onSwitch}>
                        {t('dashboard.switch_account', 'Switch Account')}
                    </button>
                </div>
            )}
        </div>
    );
}
