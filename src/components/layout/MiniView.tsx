import { useEffect, useState, useRef } from 'react';
import { Maximize2, RefreshCw, ShieldAlert, Tag, Activity, Clock } from 'lucide-react';
import { useViewStore } from '../../stores/useViewStore';
import { useAccountStore } from '../../stores/useAccountStore';
import { useConfigStore } from '../../stores/useConfigStore';
import { isTauri } from '../../utils/env';
import { useTranslation } from 'react-i18next';
import { cn } from '../../utils/cn';
import { formatTimeRemaining, formatCompactNumber } from '../../utils/format';
import { enterMiniMode, exitMiniMode } from '../../utils/windowManager';

interface ProxyRequestLog {
    id: string;
    model?: string;
    input_tokens?: number;
    output_tokens?: number;
    timestamp: number;
    status: number;
    duration: number;
    mapped_model?: string;
}

export default function MiniView() {
    const { setMiniView } = useViewStore();
    const { currentAccount, refreshQuota, fetchCurrentAccount } = useAccountStore();
    const { config } = useConfigStore();
    const { t } = useTranslation();
    const [isRefreshing, setIsRefreshing] = useState(false);
    const containerRef = useRef<HTMLDivElement>(null);
    const [latestLog, setLatestLog] = useState<ProxyRequestLog | null>(null);

    useEffect(() => {
        let unlistenFn: (() => void) | null = null;
        const setup = async () => {
            if (!isTauri()) return;
            try {
                const { listen } = await import('@tauri-apps/api/event');
                unlistenFn = await listen<ProxyRequestLog>('proxy://request', (event) => setLatestLog(event.payload));
            } catch { /* ignore */ }
        };
        setup();
        return () => { if (unlistenFn) unlistenFn(); };
    }, []);

    useEffect(() => {
        if (!config?.auto_refresh || !config?.refresh_interval || config.refresh_interval <= 0) return;
        const intervalId = setInterval(() => {
            if (!isRefreshing && currentAccount) handleRefresh();
        }, config.refresh_interval * 60 * 1000);
        return () => clearInterval(intervalId);
    }, [config?.auto_refresh, config?.refresh_interval, currentAccount, isRefreshing]);

    useEffect(() => {
        const adjustSize = async () => {
            if (isTauri() && containerRef.current) {
                await enterMiniMode(containerRef.current.scrollHeight);
            }
        };
        const timer = setTimeout(adjustSize, 50);
        return () => clearTimeout(timer);
    }, [currentAccount]);

    const handleRefresh = async () => {
        if (!currentAccount || isRefreshing) return;
        setIsRefreshing(true);
        try { await refreshQuota(currentAccount.id); await fetchCurrentAccount(); }
        finally { setTimeout(() => setIsRefreshing(false), 800); }
    };

    const handleMaximize = async () => { await exitMiniMode(); setMiniView(false); };

    const handleMouseDown = () => { if (isTauri()) import('@tauri-apps/api/window').then(({ getCurrentWindow }) => getCurrentWindow().startDragging()); };

    const geminiProModel = currentAccount?.quota?.models
        ?.filter(m => m.name.toLowerCase() === 'gemini-3-pro-high' || m.name.toLowerCase() === 'gemini-3-pro-low')
        .sort((a, b) => a.percentage - b.percentage)[0];
    const geminiFlashModel = currentAccount?.quota?.models?.find(m => m.name.toLowerCase() === 'gemini-3-flash');
    const claudeModel = currentAccount?.quota?.models
        ?.filter(m => ['claude-opus-4-6-thinking', 'claude'].includes(m.name.toLowerCase()))
        .sort((a, b) => a.percentage - b.percentage)[0];

    const renderModelRow = (model: { name: string; percentage: number; reset_time: string } | undefined, displayName: string) => {
        if (!model) return null;
        const getStatusColor = (p: number) => p >= 50 ? 'text-emerald-500' : p >= 20 ? 'text-amber-500' : 'text-rose-500';
        const getBarColor = (p: number) => p >= 50 ? 'bg-gradient-to-r from-emerald-400 to-emerald-500' : p >= 20 ? 'bg-gradient-to-r from-amber-400 to-amber-500' : 'bg-gradient-to-r from-rose-400 to-rose-500';
        return (
            <div className="space-y-1.5">
                <div className="flex justify-between items-baseline">
                    <span className="text-xs font-medium text-gray-600 dark:text-gray-400">{displayName}</span>
                    <div className="flex items-center gap-2">
                        <span className="text-[10px] text-gray-400 dark:text-gray-500 font-mono">
                            {model.reset_time ? `R: ${formatTimeRemaining(model.reset_time)}` : t('common.unknown')}
                        </span>
                        <span className={cn("text-xs font-bold", getStatusColor(model.percentage))}>{model.percentage}%</span>
                    </div>
                </div>
                <div className="w-full bg-gray-100 dark:bg-white/10 rounded-full h-1.5 overflow-hidden">
                    <div className={cn("h-full rounded-full transition-all duration-700", getBarColor(model.percentage))} style={{ width: `${model.percentage}%` }} />
                </div>
            </div>
        );
    };

    return (
        <div className="h-screen w-full flex items-center justify-center bg-transparent">
            <div ref={containerRef}
                className="w-[300px] flex flex-col bg-white/80 dark:bg-[#121212]/80 backdrop-blur-md shadow-2xl overflow-hidden border border-gray-200/50 dark:border-white/10 sm:rounded-2xl">
                <div className="flex-none flex items-center justify-between px-4 py-1 bg-gray-50/50 dark:bg-white/5 border-b border-gray-100 dark:border-white/5 select-none"
                    onMouseDown={handleMouseDown} data-tauri-drag-region>
                    <div className="flex items-center gap-2 text-sm font-semibold text-gray-900 dark:text-white overflow-hidden">
                        <div className="w-2 h-2 rounded-full bg-emerald-500 shadow-[0_0_8px_rgba(16,185,129,0.4)] animate-pulse shrink-0" />
                        <span className="truncate" title={currentAccount?.email}>{currentAccount?.email?.split('@')[0] || 'No Account'}</span>
                    </div>
                    <div className="flex items-center gap-1 shrink-0" onMouseDown={(e) => e.stopPropagation()}>
                        <button onClick={handleRefresh} className="p-2 rounded-lg hover:bg-gray-200/50 dark:hover:bg-white/10 transition-colors" title={t('common.refresh')}>
                            <RefreshCw size={14} className={cn(isRefreshing && "animate-spin text-blue-500")} />
                        </button>
                        <div className="w-px h-3 bg-gray-300 dark:bg-white/20 mx-1" />
                        <button onClick={handleMaximize} className="p-2 rounded-lg hover:bg-gray-200/50 dark:hover:bg-white/10 transition-colors text-gray-500 hover:text-gray-900 dark:text-gray-400 dark:hover:text-white">
                            <Maximize2 size={14} />
                        </button>
                    </div>
                </div>
                <div className="flex-1 overflow-y-auto overflow-x-hidden p-4 space-y-5">
                    {!currentAccount ? (
                        <div className="h-full flex flex-col items-center justify-center text-center opacity-50 space-y-2">
                            <ShieldAlert size={32} /><p className="text-sm">No account selected</p>
                        </div>
                    ) : (
                        <div className="space-y-5">
                            {currentAccount.custom_label && (
                                <div className="flex flex-wrap gap-2">
                                    <span className="flex items-center gap-1 px-2 py-0.5 rounded-md bg-orange-100 dark:bg-orange-900/30 text-orange-600 dark:text-orange-400 text-[10px] font-bold shadow-sm shrink-0">
                                        <Tag className="w-2.5 h-2.5" />{currentAccount.custom_label}
                                    </span>
                                </div>
                            )}
                            {currentAccount.custom_label && <div className="w-full h-px bg-gray-100 dark:bg-white/5" />}
                            <div className="space-y-4 !mt-0">
                                {renderModelRow(geminiProModel, 'Gemini 3 Pro')}
                                {renderModelRow(geminiFlashModel, 'Gemini 3 Flash')}
                                {renderModelRow(claudeModel, t('common.claude_series', 'Claude Series'))}
                                {!geminiProModel && !geminiFlashModel && !claudeModel && (
                                    <div className="text-center py-4 text-xs text-gray-400">No quota data available</div>
                                )}
                            </div>
                        </div>
                    )}
                </div>
                <div className="flex-none h-8 bg-gray-50 dark:bg-black/20 flex items-center justify-between px-3 text-[10px] text-gray-500 dark:text-gray-400 border-t border-gray-100 dark:border-white/5 overflow-hidden">
                    {latestLog ? (
                        <div className="flex items-center w-full gap-2">
                            <span className={`w-1.5 h-1.5 rounded-full ${latestLog.status >= 200 && latestLog.status < 400 ? 'bg-emerald-500' : 'bg-red-500'}`} />
                            <span className="font-bold truncate max-w-[100px]" title={latestLog.model}>{latestLog.mapped_model || latestLog.model}</span>
                            <div className="flex-1 flex items-center justify-end gap-2">
                                <div className="flex items-center gap-1.5 text-[9px]">
                                    <Activity size={10} className="text-blue-500" />
                                    <span className="text-gray-500 dark:text-gray-400">I:<span className="font-mono text-gray-900 dark:text-gray-200">{formatCompactNumber(latestLog.input_tokens || 0)}</span></span>
                                    <span className="text-gray-300 dark:text-gray-600">/</span>
                                    <span className="text-gray-500 dark:text-gray-400">O:<span className="font-mono text-gray-900 dark:text-gray-200">{formatCompactNumber(latestLog.output_tokens || 0)}</span></span>
                                </div>
                                <div className="w-px h-2.5 bg-gray-300 dark:bg-white/10" />
                                <div className="flex items-center gap-0.5">
                                    <Clock size={10} className="text-gray-400" />
                                    <span className="font-mono">{(latestLog.duration / 1000).toFixed(2)}s</span>
                                </div>
                            </div>
                        </div>
                    ) : (
                        <>
                            <div className="flex items-center gap-1.5"><div className="w-1.5 h-1.5 rounded-full bg-emerald-500" /><span>Connected</span></div>
                            <span className="font-mono opacity-50">v0.1.0</span>
                        </>
                    )}
                </div>
            </div>
        </div>
    );
}
