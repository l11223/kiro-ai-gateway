import { RefreshCw, Check, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface CliSyncCardProps {
    name: string;
    icon: React.ReactNode;
    isSynced: boolean;
    configPath?: string;
    onSync: () => void;
    onRestore: () => void;
    isLoading?: boolean;
}

export default function CliSyncCard({ name, icon, isSynced, configPath, onSync, onRestore, isLoading }: CliSyncCardProps) {
    const { t } = useTranslation();

    return (
        <div className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200">
            <div className="flex items-center gap-3 mb-3">
                <div className="w-10 h-10 rounded-lg bg-gray-100 dark:bg-base-200 flex items-center justify-center">{icon}</div>
                <div className="flex-1 min-w-0">
                    <h3 className="font-semibold text-sm text-gray-900 dark:text-base-content">{name}</h3>
                    {configPath && <p className="text-[10px] text-gray-400 truncate" title={configPath}>{configPath}</p>}
                </div>
                <div className={`flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-bold ${isSynced ? 'bg-green-100 dark:bg-green-900/20 text-green-600 dark:text-green-400' : 'bg-gray-100 dark:bg-base-200 text-gray-500 dark:text-gray-400'}`}>
                    {isSynced ? <Check className="w-3 h-3" /> : <X className="w-3 h-3" />}
                    {isSynced ? t('common.enabled') : t('common.disabled')}
                </div>
            </div>
            <div className="flex gap-2">
                <button onClick={onSync} disabled={isLoading}
                    className="flex-1 px-3 py-1.5 bg-blue-500 text-white text-xs font-medium rounded-lg hover:bg-blue-600 transition-colors disabled:opacity-50 flex items-center justify-center gap-1">
                    <RefreshCw className={`w-3 h-3 ${isLoading ? 'animate-spin' : ''}`} />
                    {t('proxy.sync', 'Sync')}
                </button>
                <button onClick={onRestore} disabled={isLoading}
                    className="px-3 py-1.5 text-gray-600 dark:text-gray-300 text-xs font-medium rounded-lg border border-gray-200 dark:border-base-300 hover:bg-gray-50 dark:hover:bg-base-200 transition-colors disabled:opacity-50">
                    {t('proxy.restore', 'Restore')}
                </button>
            </div>
        </div>
    );
}
