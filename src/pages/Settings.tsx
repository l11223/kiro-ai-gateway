import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Save, RefreshCw, FolderOpen } from 'lucide-react';
import { request as invoke } from '../utils/request';
import { useConfigStore } from '../stores/useConfigStore';
import { useDebugConsole } from '../stores/useDebugConsole';
import { AppConfig } from '../types/config';
import { isTauri } from '../utils/env';
import { cn } from '../utils/cn';

type TabId = 'general' | 'account' | 'advanced' | 'debug' | 'about';

function Settings() {
    const { t, i18n } = useTranslation();
    const { config, loadConfig, saveConfig, updateLanguage, updateTheme } = useConfigStore();
    const { enable, disable, isEnabled } = useDebugConsole();
    const [activeTab, setActiveTab] = useState<TabId>('general');
    const [formData, setFormData] = useState<AppConfig | null>(null);
    const [dataDirPath, setDataDirPath] = useState('');

    useEffect(() => {
        loadConfig();
        invoke<string>('get_data_dir_path').then(setDataDirPath).catch(() => {});
    }, [loadConfig]);

    useEffect(() => { if (config) setFormData(config); }, [config]);

    const handleSave = async () => {
        if (!formData) return;
        try { await saveConfig({ ...formData, auto_refresh: true }); } catch { /* handled */ }
    };

    const handleAutoLaunchToggle = async (enabled: boolean) => {
        try {
            await invoke('toggle_auto_launch', { enable: enabled });
            if (formData) setFormData({ ...formData, auto_launch: enabled });
        } catch { /* handled */ }
    };

    const tabs: { id: TabId; label: string }[] = [
        { id: 'general', label: t('settings.tabs.general', 'General') },
        { id: 'account', label: t('settings.tabs.account', 'Account') },
        { id: 'advanced', label: t('settings.tabs.advanced', 'Advanced') },
        { id: 'debug', label: t('settings.tabs.debug', 'Debug') },
        { id: 'about', label: t('settings.tabs.about', 'About') },
    ];

    if (!formData) return <div className="flex items-center justify-center h-full"><RefreshCw className="w-6 h-6 animate-spin text-gray-400" /></div>;

    return (
        <div className="h-full w-full overflow-y-auto">
            <div className="p-5 space-y-4 max-w-7xl mx-auto">
                {/* Tab bar + Save */}
                <div className="flex justify-between items-center">
                    <div className="flex items-center gap-1 bg-gray-100 dark:bg-base-200 rounded-full p-1">
                        {tabs.map(tab => (
                            <button key={tab.id} className={cn('px-6 py-2 rounded-full text-sm font-medium transition-all', activeTab === tab.id ? 'bg-gray-200 dark:bg-gray-700 text-gray-900 dark:text-gray-100 shadow-sm' : 'text-gray-600 dark:text-gray-400 hover:text-gray-900')} onClick={() => setActiveTab(tab.id)}>{tab.label}</button>
                        ))}
                    </div>
                    <button className="px-4 py-2 bg-blue-500 text-white text-sm rounded-lg hover:bg-blue-600 flex items-center gap-2 shadow-sm" onClick={handleSave}>
                        <Save className="w-4 h-4" />{t('common.save')}
                    </button>
                </div>

                {/* Content */}
                <div className="bg-white dark:bg-base-100 rounded-2xl p-6 shadow-sm border border-gray-100 dark:border-base-200">
                    {activeTab === 'general' && (
                        <div className="space-y-6">
                            <h2 className="text-lg font-semibold text-gray-900 dark:text-base-content">{t('settings.general.title', 'General Settings')}</h2>

                            {/* Language */}
                            <div>
                                <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-2">{t('settings.general.language', 'Language')}</label>
                                <select className="w-full px-4 py-3 border border-gray-200 dark:border-base-300 rounded-lg bg-gray-50 dark:bg-base-200 text-gray-900 dark:text-base-content" value={formData.language} onChange={e => { const lang = e.target.value; setFormData({ ...formData, language: lang }); i18n.changeLanguage(lang); updateLanguage(lang); }}>
                                    <option value="zh">简体中文</option><option value="zh-TW">繁體中文</option><option value="en">English</option>
                                    <option value="ja">日本語</option><option value="ko">한국어</option><option value="ru">Русский</option>
                                    <option value="es">Español</option><option value="pt">Português</option><option value="ar">العربية</option>
                                    <option value="tr">Türkçe</option><option value="vi">Tiếng Việt</option><option value="my">မြန်မာ</option>
                                </select>
                            </div>

                            {/* Theme */}
                            <div>
                                <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-2">{t('settings.general.theme', 'Theme')}</label>
                                <select className="w-full px-4 py-3 border border-gray-200 dark:border-base-300 rounded-lg bg-gray-50 dark:bg-base-200 text-gray-900 dark:text-base-content" value={formData.theme} onChange={e => { const theme = e.target.value; setFormData({ ...formData, theme }); updateTheme(theme); }}>
                                    <option value="light">{t('settings.general.theme_light', 'Light')}</option>
                                    <option value="dark">{t('settings.general.theme_dark', 'Dark')}</option>
                                    <option value="system">{t('settings.general.theme_system', 'System')}</option>
                                </select>
                            </div>

                            {/* Auto Launch */}
                            <div>
                                <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-2">{t('settings.general.auto_launch', 'Auto Launch')}</label>
                                <div className="flex items-center gap-3">
                                    <input type="checkbox" className="toggle toggle-primary" checked={formData.auto_launch || false} onChange={e => handleAutoLaunchToggle(e.target.checked)} disabled={!isTauri()} />
                                    <span className="text-sm text-gray-500">{formData.auto_launch ? t('common.enabled') : t('common.disabled')}</span>
                                </div>
                            </div>

                            {/* Menu visibility */}
                            <div>
                                <label className="block text-sm font-medium text-gray-900 dark:text-base-content mb-2">{t('settings.menu.title', 'Menu Visibility')}</label>
                                <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
                                    {[
                                        { path: '/', label: t('nav.dashboard') },
                                        { path: '/accounts', label: t('nav.accounts') },
                                        { path: '/api-proxy', label: t('nav.api_proxy') },
                                        { path: '/monitor', label: t('nav.monitor') },
                                        { path: '/token-stats', label: t('nav.token_stats') },
                                        { path: '/user-token', label: t('nav.user_token') },
                                        { path: '/security', label: t('nav.security') },
                                        { path: '/settings', label: t('nav.settings') },
                                    ].map(item => {
                                        const hidden = (formData.hidden_menu_items || []).includes(item.path);
                                        return (
                                            <label key={item.path} className={cn('flex items-center gap-2 p-3 rounded-lg border cursor-pointer transition-colors', hidden ? 'border-gray-200 dark:border-base-300 bg-gray-50 dark:bg-base-200 opacity-50' : 'border-blue-200 dark:border-blue-900/30 bg-blue-50 dark:bg-blue-900/10')}>
                                                <input type="checkbox" className="checkbox checkbox-xs checkbox-primary" checked={!hidden} onChange={() => {
                                                    const items = formData.hidden_menu_items || [];
                                                    setFormData({ ...formData, hidden_menu_items: hidden ? items.filter(i => i !== item.path) : [...items, item.path] });
                                                }} />
                                                <span className="text-sm">{item.label}</span>
                                            </label>
                                        );
                                    })}
                                </div>
                            </div>
                        </div>
                    )}

                    {activeTab === 'account' && (
                        <div className="space-y-6">
                            <h2 className="text-lg font-semibold text-gray-900 dark:text-base-content">{t('settings.tabs.account', 'Account Settings')}</h2>

                            {/* Quota Protection */}
                            <div className="p-4 bg-gray-50 dark:bg-base-200 rounded-lg">
                                <div className="flex items-center justify-between mb-3">
                                    <span className="font-medium text-sm">{t('settings.quota_protection', 'Quota Protection')}</span>
                                    <input type="checkbox" className="toggle toggle-sm toggle-primary" checked={formData.quota_protection.enabled} onChange={e => setFormData({ ...formData, quota_protection: { ...formData.quota_protection, enabled: e.target.checked } })} />
                                </div>
                                {formData.quota_protection.enabled && (
                                    <div>
                                        <label className="text-xs text-gray-500 mb-1 block">Threshold (%)</label>
                                        <input type="number" className="input input-bordered input-sm w-32" min="1" max="100" value={formData.quota_protection.threshold_percentage} onChange={e => setFormData({ ...formData, quota_protection: { ...formData.quota_protection, threshold_percentage: parseInt(e.target.value) || 10 } })} />
                                    </div>
                                )}
                            </div>

                            {/* Scheduled Warmup */}
                            <div className="p-4 bg-gray-50 dark:bg-base-200 rounded-lg">
                                <div className="flex items-center justify-between mb-3">
                                    <span className="font-medium text-sm">{t('settings.scheduled_warmup', 'Scheduled Warmup')}</span>
                                    <input type="checkbox" className="toggle toggle-sm toggle-primary" checked={formData.scheduled_warmup.enabled} onChange={e => setFormData({ ...formData, scheduled_warmup: { ...formData.scheduled_warmup, enabled: e.target.checked } })} />
                                </div>
                            </div>

                            {/* Circuit Breaker */}
                            <div className="p-4 bg-gray-50 dark:bg-base-200 rounded-lg">
                                <div className="flex items-center justify-between mb-3">
                                    <span className="font-medium text-sm">{t('settings.circuit_breaker', 'Circuit Breaker')}</span>
                                    <input type="checkbox" className="toggle toggle-sm toggle-primary" checked={formData.circuit_breaker.enabled} onChange={e => setFormData({ ...formData, circuit_breaker: { ...formData.circuit_breaker, enabled: e.target.checked } })} />
                                </div>
                                {formData.circuit_breaker.enabled && (
                                    <div>
                                        <label className="text-xs text-gray-500 mb-1 block">Backoff Steps (seconds, comma-separated)</label>
                                        <input type="text" className="input input-bordered input-sm w-full" value={formData.circuit_breaker.backoff_steps.join(', ')} onChange={e => {
                                            const steps = e.target.value.split(',').map(s => parseInt(s.trim())).filter(n => !isNaN(n));
                                            if (steps.length > 0) setFormData({ ...formData, circuit_breaker: { ...formData.circuit_breaker, backoff_steps: steps } });
                                        }} />
                                    </div>
                                )}
                            </div>
                        </div>
                    )}

                    {activeTab === 'advanced' && (
                        <div className="space-y-6">
                            <h2 className="text-lg font-semibold text-gray-900 dark:text-base-content">{t('settings.tabs.advanced', 'Advanced')}</h2>

                            <div className="flex items-center gap-3 p-4 bg-gray-50 dark:bg-base-200 rounded-lg">
                                <FolderOpen className="w-5 h-5 text-gray-500" />
                                <div>
                                    <div className="text-sm font-medium">{t('settings.advanced.data_dir', 'Data Directory')}</div>
                                    <div className="text-xs text-gray-500 font-mono">{dataDirPath || '~/.kiro_ai_gateway/'}</div>
                                </div>
                                <button className="btn btn-sm btn-ghost ml-auto" onClick={async () => { try { await invoke('open_data_folder'); } catch { /* */ } }}>Open</button>
                            </div>

                            <div className="flex items-center justify-between p-4 bg-gray-50 dark:bg-base-200 rounded-lg">
                                <div>
                                    <div className="text-sm font-medium">{t('settings.advanced.clear_logs', 'Clear Log Cache')}</div>
                                    <div className="text-xs text-gray-500">Remove cached log files</div>
                                </div>
                                <button className="btn btn-sm btn-error btn-outline" onClick={async () => { try { await invoke('clear_log_cache'); } catch { /* */ } }}>Clear</button>
                            </div>
                        </div>
                    )}

                    {activeTab === 'debug' && (
                        <div className="space-y-6">
                            <h2 className="text-lg font-semibold text-gray-900 dark:text-base-content">{t('settings.tabs.debug', 'Debug Console')}</h2>
                            <div className="flex items-center justify-between p-4 bg-gray-50 dark:bg-base-200 rounded-lg">
                                <div>
                                    <div className="text-sm font-medium">Debug Console</div>
                                    <div className="text-xs text-gray-500">Enable real-time system log streaming</div>
                                </div>
                                <input type="checkbox" className="toggle toggle-primary" checked={isEnabled} onChange={e => e.target.checked ? enable() : disable()} />
                            </div>

                            <div className="flex items-center justify-between p-4 bg-gray-50 dark:bg-base-200 rounded-lg">
                                <div>
                                    <div className="text-sm font-medium">Debug Logging</div>
                                    <div className="text-xs text-gray-500">Save full request/response chain to disk</div>
                                </div>
                                <input type="checkbox" className="toggle toggle-primary" checked={formData.proxy.debug_logging?.enabled || false} onChange={e => setFormData({ ...formData, proxy: { ...formData.proxy, debug_logging: { ...(formData.proxy.debug_logging || { enabled: false }), enabled: e.target.checked } } })} />
                            </div>
                        </div>
                    )}

                    {activeTab === 'about' && (
                        <div className="space-y-6">
                            <h2 className="text-lg font-semibold text-gray-900 dark:text-base-content">{t('settings.tabs.about', 'About')}</h2>
                            <div className="text-center py-8">
                                <div className="text-4xl mb-4">🚀</div>
                                <h3 className="text-xl font-bold text-gray-900 dark:text-base-content mb-2">Kiro AI Gateway</h3>
                                <p className="text-sm text-gray-500 mb-4">Professional AI Account Management & Protocol Proxy System</p>
                                <p className="text-xs text-gray-400">Built with Tauri v2 + React + Rust (Axum) + SQLite</p>
                            </div>
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
}

export default Settings;
