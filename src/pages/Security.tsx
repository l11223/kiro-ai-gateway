import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Shield, Lock, FileText, Activity, RefreshCw, Plus, Trash2, Search } from 'lucide-react';
import { request as invoke } from '../utils/request';
import { cn } from '../utils/cn';

interface IpAccessLog {
    id: string;
    client_ip: string;
    timestamp: number;
    method: string;
    path: string;
    status: number;
    duration: number;
    blocked: boolean;
    block_reason?: string;
    username?: string;
}

interface IpStats {
    total_requests: number;
    unique_ips: number;
    blocked_count: number;
    today_requests: number;
    blacklist_count: number;
    whitelist_count: number;
}

interface IpEntry {
    id: string;
    ip_pattern: string;
    reason?: string;
    description?: string;
    created_at: number;
    expires_at?: number;
    hit_count?: number;
}

type TabId = 'logs' | 'stats' | 'blacklist' | 'whitelist' | 'config';

const Security: React.FC = () => {
    const { t } = useTranslation();
    const [activeTab, setActiveTab] = useState<TabId>('logs');
    const [logs, setLogs] = useState<IpAccessLog[]>([]);
    const [stats, setStats] = useState<IpStats | null>(null);
    const [blacklist, setBlacklist] = useState<IpEntry[]>([]);
    const [whitelist, setWhitelist] = useState<IpEntry[]>([]);
    const [loading, setLoading] = useState(false);
    const [newIp, setNewIp] = useState('');
    const [newReason, setNewReason] = useState('');
    const [searchIp, setSearchIp] = useState('');

    const loadData = async () => {
        setLoading(true);
        try {
            const [logsData, statsData, blData, wlData] = await Promise.all([
                invoke<IpAccessLog[]>('get_ip_access_logs', { page: 1, pageSize: 100 }).catch(() => []),
                invoke<IpStats>('get_ip_stats').catch(() => null),
                invoke<IpEntry[]>('get_ip_blacklist').catch(() => []),
                invoke<IpEntry[]>('get_ip_whitelist').catch(() => []),
            ]);
            setLogs(logsData); setStats(statsData); setBlacklist(blData); setWhitelist(wlData);
        } catch { /* handled */ }
        finally { setLoading(false); }
    };

    useEffect(() => { loadData(); }, []);

    const handleAddBlacklist = async () => {
        if (!newIp) return;
        try { await invoke('add_ip_to_blacklist', { ip: newIp, reason: newReason || undefined }); setNewIp(''); setNewReason(''); await loadData(); } catch { /* */ }
    };

    const handleAddWhitelist = async () => {
        if (!newIp) return;
        try { await invoke('add_ip_to_whitelist', { ip: newIp, description: newReason || undefined }); setNewIp(''); setNewReason(''); await loadData(); } catch { /* */ }
    };

    const handleRemoveBlacklist = async (id: string) => {
        try { await invoke('remove_ip_from_blacklist', { id }); await loadData(); } catch { /* */ }
    };

    const handleRemoveWhitelist = async (id: string) => {
        try { await invoke('remove_ip_from_whitelist', { id }); await loadData(); } catch { /* */ }
    };

    const tabs = [
        { id: 'logs' as TabId, label: t('security.tab_logs', 'Access Logs'), icon: FileText },
        { id: 'stats' as TabId, label: t('security.tab_stats', 'Statistics'), icon: Activity },
        { id: 'blacklist' as TabId, label: t('security.tab_blacklist', 'Blacklist'), icon: Shield },
        { id: 'whitelist' as TabId, label: t('security.tab_whitelist', 'Whitelist'), icon: Lock },
    ];

    const filteredLogs = searchIp ? logs.filter(l => l.client_ip.includes(searchIp)) : logs;

    return (
        <div className="h-full flex flex-col p-5 gap-4 max-w-7xl mx-auto w-full">
            <div className="flex items-center justify-between">
                <h1 className="text-2xl font-bold text-gray-900 dark:text-white flex items-center gap-2">
                    <Shield className="text-blue-500" />{t('security.title')}
                </h1>
                <button onClick={loadData} className="btn btn-sm btn-ghost gap-2 text-gray-600 dark:text-gray-400">
                    <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />{t('common.refresh')}
                </button>
            </div>

            {/* Tabs */}
            <div className="bg-white dark:bg-base-100 rounded-xl shadow-sm border border-gray-100 dark:border-base-200">
                <div className="flex border-b border-gray-100 dark:border-base-200">
                    {tabs.map(tab => (
                        <button key={tab.id} onClick={() => setActiveTab(tab.id)} className={cn('flex items-center gap-2 px-6 py-4 text-sm font-medium transition-colors relative', activeTab === tab.id ? 'text-blue-600 dark:text-blue-400' : 'text-gray-500 hover:text-gray-700')}>
                            <tab.icon size={18} />{tab.label}
                            {activeTab === tab.id && <div className="absolute bottom-0 left-0 right-0 h-0.5 bg-blue-600 dark:bg-blue-400" />}
                        </button>
                    ))}
                </div>
            </div>

            {/* Content */}
            <div className="flex-1 overflow-hidden flex flex-col bg-white dark:bg-base-100 rounded-xl shadow-sm border border-gray-100 dark:border-base-200">
                {activeTab === 'logs' && (
                    <div className="flex-1 flex flex-col overflow-hidden">
                        <div className="p-4 border-b border-gray-100 dark:border-base-200">
                            <div className="relative w-64">
                                <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-gray-400" />
                                <input className="w-full pl-9 pr-3 py-2 text-sm border border-gray-200 dark:border-base-300 rounded-lg bg-white dark:bg-base-100" placeholder="Filter by IP..." value={searchIp} onChange={e => setSearchIp(e.target.value)} />
                            </div>
                        </div>
                        <div className="flex-1 overflow-auto">
                            <table className="table table-pin-rows w-full">
                                <thead><tr className="bg-gray-50/50 dark:bg-base-200/50">
                                    <th className="bg-transparent text-xs">IP</th><th className="bg-transparent text-xs">Method</th><th className="bg-transparent text-xs">Path</th><th className="bg-transparent text-xs">Status</th><th className="bg-transparent text-xs">Duration</th><th className="bg-transparent text-xs">Blocked</th><th className="bg-transparent text-xs">Time</th>
                                </tr></thead>
                                <tbody>
                                    {filteredLogs.map(log => (
                                        <tr key={log.id} className="hover:bg-gray-50/80 dark:hover:bg-base-200/50">
                                            <td className="text-xs font-mono">{log.client_ip}</td>
                                            <td><span className="badge badge-xs badge-ghost">{log.method}</span></td>
                                            <td className="text-xs truncate max-w-[200px]">{log.path}</td>
                                            <td><span className={cn('badge badge-xs', log.status < 400 ? 'badge-success' : log.status < 500 ? 'badge-warning' : 'badge-error')}>{log.status}</span></td>
                                            <td className="text-xs">{log.duration}ms</td>
                                            <td>{log.blocked && <span className="badge badge-xs badge-error">Blocked</span>}</td>
                                            <td className="text-xs text-gray-400">{new Date(log.timestamp * 1000).toLocaleString()}</td>
                                        </tr>
                                    ))}
                                    {filteredLogs.length === 0 && <tr><td colSpan={7} className="text-center py-8 text-gray-400">{t('common.empty')}</td></tr>}
                                </tbody>
                            </table>
                        </div>
                    </div>
                )}

                {activeTab === 'stats' && stats && (
                    <div className="p-6 grid grid-cols-2 md:grid-cols-3 gap-4">
                        {[
                            { label: 'Total Requests', value: stats.total_requests, color: 'blue' },
                            { label: 'Unique IPs', value: stats.unique_ips, color: 'green' },
                            { label: 'Blocked', value: stats.blocked_count, color: 'red' },
                            { label: 'Today', value: stats.today_requests, color: 'purple' },
                            { label: 'Blacklisted', value: stats.blacklist_count, color: 'orange' },
                            { label: 'Whitelisted', value: stats.whitelist_count, color: 'cyan' },
                        ].map((s, i) => (
                            <div key={i} className="bg-gray-50 dark:bg-base-200 rounded-xl p-4">
                                <div className="text-2xl font-bold text-gray-900 dark:text-base-content">{s.value}</div>
                                <div className="text-xs text-gray-500">{s.label}</div>
                            </div>
                        ))}
                    </div>
                )}

                {(activeTab === 'blacklist' || activeTab === 'whitelist') && (
                    <div className="flex-1 flex flex-col overflow-hidden">
                        <div className="p-4 border-b border-gray-100 dark:border-base-200 flex items-center gap-2">
                            <input className="input input-bordered input-sm flex-1" placeholder="IP or CIDR (e.g. 192.168.1.0/24)" value={newIp} onChange={e => setNewIp(e.target.value)} />
                            <input className="input input-bordered input-sm flex-1" placeholder={activeTab === 'blacklist' ? 'Reason' : 'Description'} value={newReason} onChange={e => setNewReason(e.target.value)} />
                            <button className="btn btn-sm btn-primary" onClick={activeTab === 'blacklist' ? handleAddBlacklist : handleAddWhitelist}><Plus className="w-4 h-4" /></button>
                        </div>
                        <div className="flex-1 overflow-auto">
                            <table className="table w-full">
                                <thead><tr className="bg-gray-50/50 dark:bg-base-200/50">
                                    <th className="bg-transparent text-xs">IP Pattern</th><th className="bg-transparent text-xs">{activeTab === 'blacklist' ? 'Reason' : 'Description'}</th>
                                    {activeTab === 'blacklist' && <th className="bg-transparent text-xs">Hits</th>}
                                    <th className="bg-transparent text-xs">Created</th><th className="bg-transparent text-xs text-right">Actions</th>
                                </tr></thead>
                                <tbody>
                                    {(activeTab === 'blacklist' ? blacklist : whitelist).map(entry => (
                                        <tr key={entry.id} className="hover:bg-gray-50/80 dark:hover:bg-base-200/50">
                                            <td className="text-sm font-mono">{entry.ip_pattern}</td>
                                            <td className="text-xs text-gray-500">{entry.reason || entry.description || '-'}</td>
                                            {activeTab === 'blacklist' && <td className="text-xs">{entry.hit_count || 0}</td>}
                                            <td className="text-xs text-gray-400">{new Date(entry.created_at * 1000).toLocaleDateString()}</td>
                                            <td className="text-right">
                                                <button className="btn btn-xs btn-ghost text-red-500" onClick={() => activeTab === 'blacklist' ? handleRemoveBlacklist(entry.id) : handleRemoveWhitelist(entry.id)}><Trash2 className="w-3 h-3" /></button>
                                            </td>
                                        </tr>
                                    ))}
                                    {(activeTab === 'blacklist' ? blacklist : whitelist).length === 0 && <tr><td colSpan={activeTab === 'blacklist' ? 5 : 4} className="text-center py-8 text-gray-400">{t('common.empty')}</td></tr>}
                                </tbody>
                            </table>
                        </div>
                    </div>
                )}
            </div>
        </div>
    );
};

export default Security;
