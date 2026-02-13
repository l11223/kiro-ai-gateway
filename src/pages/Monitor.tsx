import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Activity, RefreshCw, Search, Eye, Trash2, ToggleLeft, ToggleRight } from 'lucide-react';
import { request as invoke } from '../utils/request';
import { cn } from '../utils/cn';

interface ProxyLog {
    id: string;
    timestamp: number;
    method: string;
    url: string;
    status: number;
    duration: number;
    model?: string;
    mapped_model?: string;
    account_email?: string;
    client_ip?: string;
    error?: string;
    input_tokens?: number;
    output_tokens?: number;
    protocol?: string;
    username?: string;
}

interface ProxyStats {
    total_requests: number;
    success_count: number;
    error_count: number;
}

const Monitor: React.FC = () => {
    const { t } = useTranslation();
    const [logs, setLogs] = useState<ProxyLog[]>([]);
    const [stats, setStats] = useState<ProxyStats | null>(null);
    const [loading, setLoading] = useState(false);
    const [page, setPage] = useState(1);
    const [totalCount, setTotalCount] = useState(0);
    const [filterIp, setFilterIp] = useState('');
    const [filterModel, setFilterModel] = useState('');
    const [filterStatus, setFilterStatus] = useState('');
    const [selectedLog, setSelectedLog] = useState<ProxyLog | null>(null);
    const [monitorEnabled, setMonitorEnabled] = useState(true);
    const pageSize = 50;

    const loadData = async () => {
        setLoading(true);
        try {
            const filters: Record<string, unknown> = { page, pageSize };
            if (filterIp) filters.clientIp = filterIp;
            if (filterModel) filters.model = filterModel;
            if (filterStatus) filters.status = parseInt(filterStatus);

            const [logsData, countData, statsData] = await Promise.all([
                invoke<ProxyLog[]>('get_proxy_logs_filtered', filters),
                invoke<number>('get_proxy_logs_count_filtered', filters),
                invoke<ProxyStats>('get_proxy_stats'),
            ]);
            setLogs(logsData); setTotalCount(countData); setStats(statsData);
        } catch { /* handled */ }
        finally { setLoading(false); }
    };

    useEffect(() => { loadData(); }, [page, filterIp, filterModel, filterStatus]);

    const handleClearLogs = async () => {
        try { await invoke('clear_proxy_logs'); setLogs([]); setTotalCount(0); } catch { /* */ }
    };

    const handleToggleMonitor = async () => {
        try {
            await invoke('set_proxy_monitor_enabled', { enabled: !monitorEnabled });
            setMonitorEnabled(!monitorEnabled);
        } catch { /* */ }
    };

    const totalPages = Math.ceil(totalCount / pageSize);

    const getStatusColor = (status: number) => {
        if (status < 300) return 'badge-success';
        if (status < 400) return 'badge-info';
        if (status < 500) return 'badge-warning';
        return 'badge-error';
    };

    return (
        <div className="h-full flex flex-col p-5 gap-4 max-w-7xl mx-auto w-full">
            {/* Header */}
            <div className="flex items-center justify-between">
                <h1 className="text-2xl font-bold text-gray-900 dark:text-white flex items-center gap-2">
                    <Activity className="text-blue-500" />{t('monitor.title')}
                </h1>
                <div className="flex items-center gap-2">
                    <button onClick={handleToggleMonitor} className={cn('btn btn-sm gap-2', monitorEnabled ? 'btn-success' : 'btn-ghost')}>
                        {monitorEnabled ? <ToggleRight size={16} /> : <ToggleLeft size={16} />}
                        {monitorEnabled ? t('common.enabled') : t('common.disabled')}
                    </button>
                    <button onClick={loadData} className="btn btn-sm btn-ghost gap-2">
                        <RefreshCw size={16} className={loading ? 'animate-spin' : ''} />{t('common.refresh')}
                    </button>
                    <button onClick={handleClearLogs} className="btn btn-sm btn-ghost text-red-500 gap-2">
                        <Trash2 size={16} />{t('common.clear')}
                    </button>
                </div>
            </div>

            {/* Stats */}
            {stats && (
                <div className="grid grid-cols-3 gap-3">
                    <div className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200">
                        <div className="text-2xl font-bold text-gray-900 dark:text-base-content">{stats.total_requests}</div>
                        <div className="text-xs text-gray-500">Total Requests</div>
                    </div>
                    <div className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200">
                        <div className="text-2xl font-bold text-green-600">{stats.success_count}</div>
                        <div className="text-xs text-gray-500">Success</div>
                    </div>
                    <div className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200">
                        <div className="text-2xl font-bold text-red-600">{stats.error_count}</div>
                        <div className="text-xs text-gray-500">Errors</div>
                    </div>
                </div>
            )}

            {/* Filters */}
            <div className="flex items-center gap-2">
                <div className="relative flex-1 max-w-xs">
                    <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-gray-400" />
                    <input className="w-full pl-9 pr-3 py-2 text-sm border border-gray-200 dark:border-base-300 rounded-lg bg-white dark:bg-base-100" placeholder="Filter by IP" value={filterIp} onChange={e => { setFilterIp(e.target.value); setPage(1); }} />
                </div>
                <input className="px-3 py-2 text-sm border border-gray-200 dark:border-base-300 rounded-lg bg-white dark:bg-base-100 w-40" placeholder="Model" value={filterModel} onChange={e => { setFilterModel(e.target.value); setPage(1); }} />
                <select className="select select-bordered select-sm" value={filterStatus} onChange={e => { setFilterStatus(e.target.value); setPage(1); }}>
                    <option value="">All Status</option>
                    <option value="200">200</option>
                    <option value="429">429</option>
                    <option value="500">500</option>
                </select>
            </div>

            {/* Log table */}
            <div className="flex-1 overflow-hidden bg-white dark:bg-base-100 rounded-2xl shadow-sm border border-gray-100 dark:border-base-200 flex flex-col">
                <div className="flex-1 overflow-auto">
                    <table className="table table-pin-rows w-full">
                        <thead><tr className="bg-gray-50/50 dark:bg-base-200/50">
                            <th className="bg-transparent text-xs">Time</th>
                            <th className="bg-transparent text-xs">Method</th>
                            <th className="bg-transparent text-xs">Model</th>
                            <th className="bg-transparent text-xs">Account</th>
                            <th className="bg-transparent text-xs">IP</th>
                            <th className="bg-transparent text-xs">Status</th>
                            <th className="bg-transparent text-xs">Duration</th>
                            <th className="bg-transparent text-xs">Tokens</th>
                            <th className="bg-transparent text-xs text-right">Actions</th>
                        </tr></thead>
                        <tbody>
                            {logs.map(log => (
                                <tr key={log.id} className="hover:bg-gray-50/80 dark:hover:bg-base-200/50 group">
                                    <td className="text-[10px] text-gray-400">{new Date(log.timestamp * 1000).toLocaleTimeString()}</td>
                                    <td><span className="badge badge-xs badge-ghost">{log.method}</span></td>
                                    <td className="text-xs">
                                        <div className="text-gray-900 dark:text-base-content">{log.model || '-'}</div>
                                        {log.mapped_model && log.mapped_model !== log.model && <div className="text-[10px] text-gray-400">→ {log.mapped_model}</div>}
                                    </td>
                                    <td className="text-xs text-gray-500 truncate max-w-[120px]">{log.account_email || '-'}</td>
                                    <td className="text-xs font-mono text-gray-500">{log.client_ip || '-'}</td>
                                    <td><span className={cn('badge badge-xs', getStatusColor(log.status))}>{log.status}</span></td>
                                    <td className="text-xs">{log.duration}ms</td>
                                    <td className="text-[10px] text-gray-400">
                                        {log.input_tokens ? `↑${log.input_tokens}` : ''}{log.output_tokens ? ` ↓${log.output_tokens}` : ''}
                                    </td>
                                    <td className="text-right">
                                        <button className="btn btn-xs btn-ghost opacity-0 group-hover:opacity-100" onClick={() => setSelectedLog(log)}><Eye className="w-3 h-3" /></button>
                                    </td>
                                </tr>
                            ))}
                            {logs.length === 0 && <tr><td colSpan={9} className="text-center py-12 text-gray-400">{t('common.empty')}</td></tr>}
                        </tbody>
                    </table>
                </div>
            </div>

            {/* Pagination */}
            {totalPages > 1 && (
                <div className="flex items-center justify-center gap-2">
                    <button className="btn btn-xs btn-ghost" disabled={page <= 1} onClick={() => setPage(p => p - 1)}>{t('common.prev_page')}</button>
                    <span className="text-xs text-gray-500">{page} / {totalPages} ({totalCount} total)</span>
                    <button className="btn btn-xs btn-ghost" disabled={page >= totalPages} onClick={() => setPage(p => p + 1)}>{t('common.next_page')}</button>
                </div>
            )}

            {/* Detail modal */}
            {selectedLog && (
                <div className="modal modal-open">
                    <div className="modal-box max-w-2xl">
                        <h3 className="font-bold text-lg mb-4">Request Detail</h3>
                        <div className="space-y-2 text-sm">
                            <div className="grid grid-cols-2 gap-2">
                                <div><span className="text-gray-500">URL:</span> <span className="font-mono text-xs break-all">{selectedLog.url}</span></div>
                                <div><span className="text-gray-500">Method:</span> {selectedLog.method}</div>
                                <div><span className="text-gray-500">Status:</span> <span className={cn('badge badge-xs', getStatusColor(selectedLog.status))}>{selectedLog.status}</span></div>
                                <div><span className="text-gray-500">Duration:</span> {selectedLog.duration}ms</div>
                                <div><span className="text-gray-500">Model:</span> {selectedLog.model || '-'}</div>
                                <div><span className="text-gray-500">Mapped:</span> {selectedLog.mapped_model || '-'}</div>
                                <div><span className="text-gray-500">Account:</span> {selectedLog.account_email || '-'}</div>
                                <div><span className="text-gray-500">Client IP:</span> {selectedLog.client_ip || '-'}</div>
                                <div><span className="text-gray-500">Protocol:</span> {selectedLog.protocol || '-'}</div>
                                <div><span className="text-gray-500">Username:</span> {selectedLog.username || '-'}</div>
                            </div>
                            {selectedLog.error && <div className="bg-red-50 dark:bg-red-900/20 p-3 rounded-lg text-red-600 text-xs">{selectedLog.error}</div>}
                        </div>
                        <div className="modal-action"><button className="btn" onClick={() => setSelectedLog(null)}>{t('common.close')}</button></div>
                    </div>
                </div>
            )}
        </div>
    );
};

export default Monitor;
