import { Clock, Lock } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { cn } from '../../utils/cn';
import { getQuotaColor, formatTimeRemaining, getTimeRemainingColor } from '../../utils/format';

interface QuotaItemProps {
    label: string;
    percentage: number;
    resetTime?: string;
    isProtected?: boolean;
    className?: string;
}

export function QuotaItem({ label, percentage, resetTime, isProtected, className }: QuotaItemProps) {
    const { t } = useTranslation();
    const getBgColor = (p: number) => {
        const c = getQuotaColor(p);
        return c === 'success' ? 'bg-emerald-500' : c === 'warning' ? 'bg-amber-500' : 'bg-rose-500';
    };
    const getTextColor = (p: number) => {
        const c = getQuotaColor(p);
        return c === 'success' ? 'text-emerald-600 dark:text-emerald-400' : c === 'warning' ? 'text-amber-600 dark:text-amber-400' : 'text-rose-600 dark:text-rose-400';
    };
    const getTimeColor = (time?: string) => {
        if (!time) return 'text-gray-300 dark:text-gray-600';
        const c = getTimeRemainingColor(time);
        return c === 'success' ? 'text-emerald-600 dark:text-emerald-400' : c === 'warning' ? 'text-amber-600 dark:text-amber-400' : 'text-gray-400 dark:text-gray-500 opacity-60';
    };

    return (
        <div className={cn("relative h-[22px] flex items-center px-1.5 rounded-md overflow-hidden border border-gray-100/50 dark:border-white/5 bg-gray-50/30 dark:bg-white/5", className)}>
            <div className={cn("absolute inset-y-0 left-0 transition-all duration-700 ease-out opacity-15 dark:opacity-20", getBgColor(percentage))} style={{ width: `${percentage}%` }} />
            <div className="relative z-10 w-full flex items-center text-[10px] font-mono leading-none gap-1.5">
                <span className="flex-1 min-w-0 text-gray-500 dark:text-gray-400 font-bold truncate text-left" title={label}>{label}</span>
                <div className="w-[58px] flex justify-start shrink-0">
                    {resetTime ? (
                        <span className={cn("flex items-center gap-0.5 font-medium truncate", getTimeColor(resetTime))}>
                            <Clock className="w-2.5 h-2.5 shrink-0" />{formatTimeRemaining(resetTime)}
                        </span>
                    ) : <span className="text-gray-300 dark:text-gray-600 italic scale-90">N/A</span>}
                </div>
                <span className={cn("w-[28px] text-right font-bold flex items-center justify-end gap-0.5 shrink-0", getTextColor(percentage))}>
                    {isProtected && <span title={t('accounts.quota_protected', 'Protected')}><Lock className="w-2.5 h-2.5 text-amber-500" /></span>}
                    {percentage}%
                </span>
            </div>
        </div>
    );
}
