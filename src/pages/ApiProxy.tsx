import { useState, useEffect, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { request as invoke } from '../utils/request';
import { copyToClipboard } from '../utils/clipboard';
import { Power, Copy, RefreshCw, Settings, Plus, Check, X, ChevronDown, Globe, Shield, Zap } from 'lucide-react';
import { AppConfig, ProxyConfig, ExperimentalConfig, SchedulingMode } from '../types/config';
import { cn } from '../utils/cn';

interface ProxyStatus {
    running: boolean;
    port: number;
    base_url: string;
    active_accounts: number;
}

function CollapsibleCard({ title, icon, enabled, onToggle, children, defaultExpanded = false }: {
    title: string; icon: React.ReactNode; enabled?: boolean; onToggle?: (v: boolean) => void; children: React.ReactNode; defaultExpanded?: boolean;
}) {
    const [isExpanded, setIsExpanded] = useState(defaultExpanded);
    const { t } = useTranslation();
    return (
        <div className="bg-white dark:bg-base-100 rounded-xl shadow-sm border border-gray-100 dark:border-gray-700/50 overflow-hidden hover:shadow-md transition-all">
            <div className="px-5 py-4 flex items-center justify-between cursor-pointer bg-gray-50/50 dark:bg-gray-800/50 hover:bg-gray-50 dark:hover:bg-gray-700/50" onClick={e => { if (!(e.target as HTMLElement).closest('.no-expand')) setIsExpanded(!isExpanded); }}>
                <div className="flex items-center gap-3">
                    <div className="text-gray-500 dark:text-gray-400">{icon}</div>
                    <span className="font-medium text-sm text-gray-900 dark:text-gray-100">{title}</span>
                    {enabled !== undefined && <div className={cn('text-xs px-2 py-0.5 rounded-full', enabled ? 'bg-green-100 text-green-700 dark:bg-green-900/40 dark:text-green-400' : 'bg-gray-100 text-gray-500')}>{enabled ? t('common.enabled') : t('common.disabled')}</div>}
                </div>
                <div className="flex items-center gap-4 no-expand">
                    {enabled !== undefined && onToggle && <input type="checkbox" className="toggle toggle-sm" checked={enabled} onChange={e => { e.stopPropagation(); onToggle(e.target.checked); }} onClick={e => e.stopPropagation()} />}
                    <ChevronDown className={cn('w-4 h-4 transition-transform', isExpanded && 'rotate-180')} />
                </div>
            </div>
            <div className={cn('transition-all duration-300 border-t border-gray-100 dark:border-base-200', isExpanded ? 'max-h-[2000px] opacity-100' : 'max-h-0 opacity-0 overflow-hidden')}>
                <div className="p-5">{children}</div>
            </div>
        </div>
    );
}

export default function ApiProxy() {
    const { t } = useTranslation();
    const [status, setStatus] = useState<ProxyStatus>({ running: false, port: 0, base_url: '', active_accounts: 0 });
    const [appConfig, setAppConfig] = useState<AppConfig | null>(null);
    const [configLoading, setConfigLoading] = useState(true);
    const [loading, setLoading] = useState(false);
    const [copied, setCopied] = useState<string | null>(null);
    const [selectedProtocol, setSelectedProtocol] = useState<'openai' | 'anthropic' | 'gemini'>('openai');
    const [newMappingFrom, setNewMappingFrom] = useState('');
    const [newMappingTo, setNewMappingTo] = useState('');

    useEffect(() => {
        loadConfig();
        loadStatus();
        const interval = setInterval(loadStatus, 3000);
        return () => clearInterval(interval);
    }, []);

    const loadConfig = async () => {
        setConfigLoading(true);
        try { const config = await invoke<AppConfig>('load_config'); setAppConfig(config); } catch (e) { console.error('Load config failed:', e); }
        finally { setConfigLoading(false); }
    };

    const loadStatus = async () => {
        try { const s = await invoke<ProxyStatus>('get_proxy_status'); setStatus(s); } catch { /* ignore */ }
    };

    const saveConfig = async (newConfig: AppConfig) => {
        setAppConfig(newConfig);
        try { await invoke('save_config', { config: newConfig }); } catch (e) { console.error('Save config failed:', e); }
    };

    const handleToggleProxy = async () => {
        setLoading(true);
        try {
            if (status.running) { await invoke('stop_proxy_service'); }
            else { await invoke('start_proxy_service'); }
            await loadStatus();
        } catch (e) { console.error('Toggle proxy failed:', e); }
        finally { setLoading(false); }
    };

    const handleCopy = async (text: string, key: string) => {
        const ok = await copyToClipboard(text);
        if (ok) { setCopied(key); setTimeout(() => setCopied(null), 2000); }
    };

    const updateProxyConfig = (updates: Partial<ProxyConfig>) => {
        if (!appConfig) return;
        saveConfig({ ...appConfig, proxy: { ...appConfig.proxy, ...updates } });
    };

    const handleAddMapping = async () => {
        if (!appConfig || !newMappingFrom || !newMappingTo) return;
        const newMapping = { ...(appConfig.proxy.custom_mapping || {}), [newMappingFrom]: newMappingTo };
        const newConfig = { ...appConfig.proxy, custom_mapping: newMapping };
        try {
            await invoke('update_model_mapping', { config: newConfig });
            setAppConfig({ ...appConfig, proxy: newConfig });
            setNewMappingFrom(''); setNewMappingTo('');
        } catch (e) { console.error('Add mapping failed:', e); }
    };

    const handleRemoveMapping = async (key: string) => {
        if (!appConfig?.proxy.custom_mapping) return;
        const newMapping = { ...appConfig.proxy.custom_mapping };
        delete newMapping[key];
        const newConfig = { ...appConfig.proxy, custom_mapping: newMapping };
        try {
            await invoke('update_model_mapping', { config: newConfig });
            setAppConfig({ ...appConfig, proxy: newConfig });
        } catch (e) { console.error('Remove mapping failed:', e); }
    };

    const protocolEndpoints = useMemo(() => {
        const base = status.base_url || `http://127.0.0.1:${appConfig?.proxy.port || 8080}`;
        return {
            openai: { url: `${base}/v1/chat/completions`, models: `${base}/v1/models` },
            anthropic: { url: `${base}/v1/messages`, models: '' },
            gemini: { url: `${base}/v1beta/models`, models: '' },
        };
    }, [status.base_url, appConfig?.proxy.port]);

    if (configLoading) return <div className="flex items-center justify-center h-full"><RefreshCw className="w-6 h-6 animate-spin text-gray-400" /></div>;

    return (
        <div className="h-full w-full overflow-y-auto">
            <div className="p-5 space-y-4 max-w-7xl mx-auto">
                {/* Status bar */}
                <div className="bg-white dark:bg-base-100 rounded-xl p-5 shadow-sm border border-gray-100 dark:border-base-200">
                    <div className="flex items-center justify-between mb-4">
                        <div className="flex items-center gap-3">
                            <div className={cn('w-3 h-3 rounded-full', status.running ? 'bg-green-500 animate-pulse' : 'bg-gray-300')} />
                            <span className="font-semibold text-gray-900 dark:text-base-content">{status.running ? t('proxy.running', 'Proxy Running') : t('proxy.stopped', 'Proxy Stopped')}</span>
                            {status.running && <span className="text-xs text-gray-500">Port {status.port} · {status.active_accounts} accounts</span>}
                        </div>
                        <button className={cn('px-4 py-2 rounded-lg text-sm font-medium flex items-center gap-2 transition-colors', status.running ? 'bg-red-500 hover:bg-red-600 text-white' : 'bg-green-500 hover:bg-green-600 text-white', loading && 'opacity-70')} onClick={handleToggleProxy} disabled={loading}>
                            <Power className="w-4 h-4" />
                            {status.running ? t('proxy.stop', 'Stop') : t('proxy.start', 'Start')}
                        </button>
                    </div>

                    {/* Endpoints */}
                    <div className="space-y-3">
                        <div className="flex gap-1 bg-gray-100 dark:bg-base-200 p-1 rounded-lg w-fit">
                            {(['openai', 'anthropic', 'gemini'] as const).map(p => (
                                <button key={p} className={cn('px-3 py-1.5 rounded-md text-xs font-medium transition-colors', selectedProtocol === p ? 'bg-white dark:bg-base-100 text-blue-600 shadow-sm' : 'text-gray-500')} onClick={() => setSelectedProtocol(p)}>{p.charAt(0).toUpperCase() + p.slice(1)}</button>
                            ))}
                        </div>
                        <div className="flex items-center gap-2 bg-gray-50 dark:bg-base-200 rounded-lg p-3">
                            <code className="flex-1 text-sm font-mono text-gray-700 dark:text-gray-300 truncate">{protocolEndpoints[selectedProtocol].url}</code>
                            <button className="btn btn-xs btn-ghost" onClick={() => handleCopy(protocolEndpoints[selectedProtocol].url, 'endpoint')}>
                                {copied === 'endpoint' ? <Check className="w-3.5 h-3.5 text-green-500" /> : <Copy className="w-3.5 h-3.5" />}
                            </button>
                        </div>
                        {appConfig?.proxy.api_key && (
                            <div className="flex items-center gap-2 bg-gray-50 dark:bg-base-200 rounded-lg p-3">
                                <span className="text-xs text-gray-500 shrink-0">API Key:</span>
                                <code className="flex-1 text-sm font-mono text-gray-700 dark:text-gray-300 truncate">{appConfig.proxy.api_key}</code>
                                <button className="btn btn-xs btn-ghost" onClick={() => handleCopy(appConfig.proxy.api_key, 'apikey')}>
                                    {copied === 'apikey' ? <Check className="w-3.5 h-3.5 text-green-500" /> : <Copy className="w-3.5 h-3.5" />}
                                </button>
                            </div>
                        )}
                    </div>
                </div>

                {/* Proxy Config */}
                <CollapsibleCard title={t('proxy.config', 'Proxy Configuration')} icon={<Settings className="w-5 h-5" />} defaultExpanded>
                    <div className="space-y-4">
                        <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                            <div>
                                <label className="text-sm font-medium text-gray-700 dark:text-gray-300 mb-1 block">Port</label>
                                <input type="number" className="input input-bordered input-sm w-full" value={appConfig?.proxy.port || 8080} onChange={e => updateProxyConfig({ port: parseInt(e.target.value) || 8080 })} />
                            </div>
                            <div>
                                <label className="text-sm font-medium text-gray-700 dark:text-gray-300 mb-1 block">Auth Mode</label>
                                <select className="select select-bordered select-sm w-full" value={appConfig?.proxy.auth_mode || 'auto'} onChange={e => updateProxyConfig({ auth_mode: e.target.value as ProxyConfig['auth_mode'] })}>
                                    <option value="off">Off</option>
                                    <option value="strict">Strict</option>
                                    <option value="all_except_health">All Except Health</option>
                                    <option value="auto">Auto</option>
                                </select>
                            </div>
                            <div>
                                <label className="text-sm font-medium text-gray-700 dark:text-gray-300 mb-1 block">Request Timeout (s)</label>
                                <input type="number" className="input input-bordered input-sm w-full" value={appConfig?.proxy.request_timeout || 120} onChange={e => updateProxyConfig({ request_timeout: parseInt(e.target.value) || 120 })} />
                            </div>
                            <div className="flex items-center gap-3">
                                <label className="text-sm font-medium text-gray-700 dark:text-gray-300">LAN Access</label>
                                <input type="checkbox" className="toggle toggle-sm toggle-primary" checked={appConfig?.proxy.allow_lan_access || false} onChange={e => updateProxyConfig({ allow_lan_access: e.target.checked })} />
                            </div>
                        </div>
                        <div className="flex items-center gap-3">
                            <label className="text-sm font-medium text-gray-700 dark:text-gray-300">Enable Logging</label>
                            <input type="checkbox" className="toggle toggle-sm toggle-primary" checked={appConfig?.proxy.enable_logging || false} onChange={e => updateProxyConfig({ enable_logging: e.target.checked })} />
                        </div>
                    </div>
                </CollapsibleCard>

                {/* Scheduling */}
                <CollapsibleCard title={t('proxy.scheduling', 'Scheduling')} icon={<Zap className="w-5 h-5" />}>
                    <div className="space-y-4">
                        <div>
                            <label className="text-sm font-medium text-gray-700 dark:text-gray-300 mb-1 block">Mode</label>
                            <select className="select select-bordered select-sm w-full" value={appConfig?.proxy.scheduling?.mode || 'Balance'} onChange={e => {
                                if (!appConfig) return;
                                saveConfig({ ...appConfig, proxy: { ...appConfig.proxy, scheduling: { ...(appConfig.proxy.scheduling || { mode: 'Balance', max_wait_seconds: 60 }), mode: e.target.value as SchedulingMode } } });
                            }}>
                                <option value="CacheFirst">Cache First (Sticky)</option>
                                <option value="Balance">Balance</option>
                                <option value="PerformanceFirst">Performance First (Round Robin)</option>
                            </select>
                        </div>
                        <div>
                            <label className="text-sm font-medium text-gray-700 dark:text-gray-300 mb-1 block">Max Wait (s)</label>
                            <input type="number" className="input input-bordered input-sm w-32" value={appConfig?.proxy.scheduling?.max_wait_seconds || 60} onChange={e => {
                                if (!appConfig) return;
                                saveConfig({ ...appConfig, proxy: { ...appConfig.proxy, scheduling: { ...(appConfig.proxy.scheduling || { mode: 'Balance', max_wait_seconds: 60 }), max_wait_seconds: parseInt(e.target.value) || 60 } } });
                            }} />
                        </div>
                        <div className="flex gap-2">
                            <button className="btn btn-sm btn-outline" onClick={async () => { try { await invoke('clear_proxy_session_bindings'); } catch { /* */ } }}>Clear Session Bindings</button>
                            <button className="btn btn-sm btn-outline" onClick={async () => { try { await invoke('clear_all_proxy_rate_limits'); } catch { /* */ } }}>Clear Rate Limits</button>
                        </div>
                    </div>
                </CollapsibleCard>

                {/* Model Mapping */}
                <CollapsibleCard title={t('proxy.model_mapping', 'Model Mapping')} icon={<Globe className="w-5 h-5" />}>
                    <div className="space-y-3">
                        {Object.entries(appConfig?.proxy.custom_mapping || {}).map(([from, to]) => (
                            <div key={from} className="flex items-center gap-2 bg-gray-50 dark:bg-base-200 rounded-lg p-2">
                                <code className="text-xs font-mono flex-1 truncate">{from}</code>
                                <span className="text-gray-400">→</span>
                                <code className="text-xs font-mono flex-1 truncate">{to}</code>
                                <button className="btn btn-xs btn-ghost text-red-500" onClick={() => handleRemoveMapping(from)}><X className="w-3 h-3" /></button>
                            </div>
                        ))}
                        <div className="flex items-center gap-2">
                            <input className="input input-bordered input-xs flex-1" placeholder="From model" value={newMappingFrom} onChange={e => setNewMappingFrom(e.target.value)} />
                            <span className="text-gray-400">→</span>
                            <input className="input input-bordered input-xs flex-1" placeholder="To model" value={newMappingTo} onChange={e => setNewMappingTo(e.target.value)} />
                            <button className="btn btn-xs btn-primary" onClick={handleAddMapping}><Plus className="w-3 h-3" /></button>
                        </div>
                    </div>
                </CollapsibleCard>

                {/* Experimental */}
                <CollapsibleCard title={t('proxy.experimental', 'Experimental')} icon={<Shield className="w-5 h-5" />} enabled={appConfig?.proxy.experimental?.enable_usage_scaling} onToggle={v => {
                    if (!appConfig) return;
                    saveConfig({ ...appConfig, proxy: { ...appConfig.proxy, experimental: { ...(appConfig.proxy.experimental || { enable_usage_scaling: false }), enable_usage_scaling: v } } });
                }}>
                    <div className="space-y-3">
                        <p className="text-xs text-gray-500">Context compression and usage scaling features.</p>
                        <div className="grid grid-cols-3 gap-3">
                            {(['l1', 'l2', 'l3'] as const).map(level => {
                                const key = `context_compression_threshold_${level}` as keyof ExperimentalConfig;
                                const defaults = { l1: 0.4, l2: 0.55, l3: 0.7 };
                                return (
                                    <div key={level}>
                                        <label className="text-xs text-gray-500 mb-1 block">{level.toUpperCase()} Threshold</label>
                                        <input type="number" step="0.05" min="0" max="1" className="input input-bordered input-xs w-full" value={(appConfig?.proxy.experimental as Record<string, number | boolean> | undefined)?.[key] as number ?? defaults[level]} onChange={e => {
                                            if (!appConfig) return;
                                            saveConfig({ ...appConfig, proxy: { ...appConfig.proxy, experimental: { ...(appConfig.proxy.experimental || { enable_usage_scaling: false }), [key]: parseFloat(e.target.value) } } });
                                        }} />
                                    </div>
                                );
                            })}
                        </div>
                    </div>
                </CollapsibleCard>
            </div>
        </div>
    );
}
