import { ArrowRightLeft, RefreshCw, Trash2, Download, Info, Ban, Diamond, Gem, Circle, ToggleLeft, ToggleRight, Tag } from 'lucide-react';
import type { Account } from '../../types/account';
import { cn } from '../../utils/cn';
import { useTranslation } from 'react-i18next';
import { QuotaItem } from './QuotaItem';

interface AccountCardProps {
    account: Account;
    selected: boolean;
    onSelect: () => void;
    isCurrent: boolean;
    isRefreshing: boolean;
    onSwitch: () => void;
    onRefresh: () => void;
    onViewDetails: () => void;
    onExport: () => void;
    onDelete: () => void;
    onToggleProxy: () => void;
}

export default function AccountCard({
    account, selected, onSelect, isCurrent, isRefreshing,
    onSwitch, onRefresh, onViewDetails, onExport, onDelete, onToggleProxy
}: AccountCardProps) {
    const { t } = useTranslation();
    const isDisabled = Boolean(account.disabled);
    const displayModels = account.quota?.models || [];

    const tier = account.quota?.subscription_tier?.toLowerCase() || '';
    const tierBadge = tier.includes('ultra')
        ? <span className="flex items-center gap-1 px-1.5 py-0.5 rounded-md bg-gradient-to-r from-purple-600 to-pink-600 text-white text-[9px] font-bold shadow-sm"><Gem className="w-2.5 h-2.5 fill-current" />ULTRA</span>
        : tier.includes('pro')
            ? <span className="flex items-center gap-1 px-1.5 py-0.5 rounded-md bg-gradient-to-r from-blue-600 to-indigo-600 text-white text-[9px] font-bold shadow-sm"><Diamond className="w-2.5 h-2.5 fill-current" />PRO</span>
            : tier
                ? <span className="flex items-center gap-1 px-1.5 py-0.5 rounded-md bg-gray-100 dark:bg-white/10 text-gray-500 dark:text-gray-400 text-[9px] font-bold shadow-sm border border-gray-200 dark:border-white/10"><Circle className="w-2.5 h-2.5" />FREE</span>
                : null;

    return (
        <div className={cn(
            "flex flex-col p-3 rounded-xl border transition-all hover:shadow-md",
            isCurrent ? "bg-blue-50/30 border-blue-200 dark:bg-blue-900/10 dark:border-blue-900/30" : "bg-white dark:bg-base-100 border-gray-200 dark:border-base-300",
            (isRefreshing || isDisabled) && "opacity-70"
        )}>
            <div className="flex-none flex items-start gap-3 mb-2">
                <input type="checkbox" className="mt-1 checkbox checkbox-xs rounded" checked={selected} onChange={onSelect} onClick={(e) => e.stopPropagation()} />
                <div className="flex-1 min-w-0 flex flex-col gap-1.5">
                    <h3 className={cn("font-semibold text-sm truncate w-full", isCurrent ? "text-blue-700 dark:text-blue-400" : "text-gray-900 dark:text-base-content")} title={account.email}>
                        {account.email}
                    </h3>
                    <div className="flex items-center gap-1.5 flex-wrap">
                        {isCurrent && <span className="px-1.5 py-0.5 rounded-md bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300 text-[9px] font-bold">{t('accounts.current', 'CURRENT').toUpperCase()}</span>}
                        {isDisabled && <span className="px-1.5 py-0.5 rounded-md bg-rose-100 dark:bg-rose-900/40 text-rose-700 dark:text-rose-300 text-[9px] font-bold flex items-center gap-1"><Ban className="w-2.5 h-2.5" />{t('common.disabled').toUpperCase()}</span>}
                        {account.proxy_disabled && <span className="px-1.5 py-0.5 rounded-md bg-orange-100 dark:bg-orange-900/40 text-orange-700 dark:text-orange-300 text-[9px] font-bold flex items-center gap-1"><Ban className="w-2.5 h-2.5" />PROXY OFF</span>}
                        {tierBadge}
                        {account.custom_label && <span className="flex items-center gap-1 px-1.5 py-0.5 rounded-md bg-orange-100 dark:bg-orange-900/40 text-orange-700 dark:text-orange-300 text-[9px] font-bold"><Tag className="w-2.5 h-2.5" />{account.custom_label}</span>}
                    </div>
                </div>
            </div>
            <div className="flex-1 px-2 mb-2">
                {account.quota?.is_forbidden ? (
                    <div className="flex items-center gap-2 text-xs text-red-500 dark:text-red-400 bg-red-50/50 dark:bg-red-900/10 p-2 rounded-lg border border-red-100 dark:border-red-900/30">
                        <Ban className="w-4 h-4 shrink-0" /><span>Forbidden</span>
                    </div>
                ) : (
                    <div className="grid grid-cols-1 gap-2 content-start">
                        {displayModels.map(m => (
                            <QuotaItem key={m.name} label={m.name} percentage={m.percentage} resetTime={m.reset_time}
                                isProtected={account.protected_models?.includes(m.name)} />
                        ))}
                    </div>
                )}
            </div>
            <div className="flex-none flex items-center justify-center pt-2 pb-1 border-t border-gray-100 dark:border-base-200">
                <div className="flex flex-wrap items-center justify-center gap-1 w-full">
                    <button className="p-1.5 text-gray-400 hover:text-sky-600 dark:hover:text-sky-400 hover:bg-sky-50 dark:hover:bg-sky-900/30 rounded-lg transition-all" onClick={(e) => { e.stopPropagation(); onViewDetails(); }} title={t('common.details')}><Info className="w-3.5 h-3.5" /></button>
                    <button className={`p-1.5 rounded-lg transition-all ${isDisabled ? 'cursor-not-allowed' : 'text-gray-400 hover:text-blue-600 dark:hover:text-blue-400 hover:bg-blue-50 dark:hover:bg-blue-900/30'}`} onClick={(e) => { e.stopPropagation(); onSwitch(); }} disabled={isDisabled} title={t('accounts.switch', 'Switch')}><ArrowRightLeft className="w-3.5 h-3.5" /></button>
                    <button className={`p-1.5 rounded-lg transition-all ${isRefreshing ? 'text-green-600 bg-green-50' : 'text-gray-400 hover:text-green-600 hover:bg-green-50'}`} onClick={(e) => { e.stopPropagation(); onRefresh(); }} disabled={isRefreshing || isDisabled} title={t('common.refresh')}><RefreshCw className={`w-3.5 h-3.5 ${isRefreshing ? 'animate-spin' : ''}`} /></button>
                    <button className="p-1.5 text-gray-400 hover:text-indigo-600 hover:bg-indigo-50 rounded-lg transition-all" onClick={(e) => { e.stopPropagation(); onExport(); }} title={t('common.export')}><Download className="w-3.5 h-3.5" /></button>
                    <button className={cn("p-1.5 rounded-lg transition-all", account.proxy_disabled ? "text-gray-400 hover:text-green-600 hover:bg-green-50" : "text-gray-400 hover:text-orange-600 hover:bg-orange-50")} onClick={(e) => { e.stopPropagation(); onToggleProxy(); }}>
                        {account.proxy_disabled ? <ToggleRight className="w-3.5 h-3.5" /> : <ToggleLeft className="w-3.5 h-3.5" />}
                    </button>
                    <button className="p-1.5 text-gray-400 hover:text-red-600 hover:bg-red-50 rounded-lg transition-all" onClick={(e) => { e.stopPropagation(); onDelete(); }} title={t('common.delete')}><Trash2 className="w-3.5 h-3.5" /></button>
                </div>
            </div>
        </div>
    );
}
