import { Shield, Globe, Ban, Clock } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface IpStats {
    total_requests: number;
    unique_ips: number;
    blocked_count: number;
    today_requests: number;
    blacklist_count: number;
    whitelist_count: number;
}

interface IpStatisticsProps {
    stats: IpStats | null;
}

export default function IpStatistics({ stats }: IpStatisticsProps) {
    const { t } = useTranslation();
    if (!stats) return null;

    const items = [
        { icon: Globe, label: t('security.total_requests', 'Total Requests'), value: stats.total_requests, color: 'text-blue-500' },
        { icon: Shield, label: t('security.unique_ips', 'Unique IPs'), value: stats.unique_ips, color: 'text-green-500' },
        { icon: Ban, label: t('security.blocked', 'Blocked'), value: stats.blocked_count, color: 'text-red-500' },
        { icon: Clock, label: t('security.today', 'Today'), value: stats.today_requests, color: 'text-amber-500' },
    ];

    return (
        <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            {items.map(({ icon: Icon, label, value, color }) => (
                <div key={label} className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200">
                    <div className="flex items-center gap-2 mb-2">
                        <Icon className={`w-4 h-4 ${color}`} />
                        <span className="text-xs text-gray-500 dark:text-gray-400">{label}</span>
                    </div>
                    <div className="text-2xl font-bold text-gray-900 dark:text-base-content">{value.toLocaleString()}</div>
                </div>
            ))}
        </div>
    );
}
