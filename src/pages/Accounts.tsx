import {
    Download,
    LayoutGrid,
    List,
    RefreshCw,
    Search,
    Sparkles,
    ToggleLeft,
    ToggleRight,
    Trash2,
    Upload,
    Tag,
} from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useAccountStore } from '../stores/useAccountStore';
import { useConfigStore } from '../stores/useConfigStore';
import { Account } from '../types/account';
import { cn } from '../utils/cn';
import { isTauri } from '../utils/env';
import { request as invoke } from '../utils/request';
import { exportAccounts } from '../services/accountService';

type FilterType = 'all' | 'pro' | 'ultra' | 'free';
type ViewMode = 'list' | 'grid';

function Accounts() {
    const { t } = useTranslation();
    const {
        accounts,
        currentAccount,
        fetchAccounts,
        addAccount,
        deleteAccount,
        deleteAccounts,
        switchAccount,
        loading,
        refreshQuota,
        refreshAllQuotas,
        toggleProxyStatus,
        warmUpAccounts,
        warmUpAccount,
        updateAccountLabel,
    } = useAccountStore();
    const { showAllQuotas, toggleShowAllQuotas } = useConfigStore();

    const [searchQuery, setSearchQuery] = useState('');
    const [filter, setFilter] = useState<FilterType>('all');
    const [viewMode, setViewMode] = useState<ViewMode>(() => {
        const saved = localStorage.getItem('accounts_view_mode');
        return (saved === 'list' || saved === 'grid') ? saved : 'list';
    });
    const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
    const [deleteConfirmId, setDeleteConfirmId] = useState<string | null>(null);
    const [isBatchDelete, setIsBatchDelete] = useState(false);
    const [isRefreshing, setIsRefreshing] = useState(false);
    const [isWarmuping, setIsWarmuping] = useState(false);
    const [refreshingIds, setRefreshingIds] = useState<Set<string>>(new Set());
    const [switchingAccountId, setSwitchingAccountId] = useState<string | null>(null);
    const [currentPage, setCurrentPage] = useState(1);
    const [editingLabelId, setEditingLabelId] = useState<string | null>(null);
    const [editingLabelValue, setEditingLabelValue] = useState('');
    const fileInputRef = useRef<HTMLInputElement>(null);

    const ITEMS_PER_PAGE = viewMode === 'grid' ? 12 : 15;

    useEffect(() => {
        localStorage.setItem('accounts_view_mode', viewMode);
    }, [viewMode]);

    useEffect(() => { fetchAccounts(); }, []);
    useEffect(() => { setCurrentPage(1); }, [viewMode]);
    useEffect(() => { setSelectedIds(new Set()); setCurrentPage(1); }, [filter, searchQuery]);

    // Filtering logic
    const searchedAccounts = useMemo(() => {
        if (!searchQuery) return accounts;
        const q = searchQuery.toLowerCase();
        return accounts.filter(a => a.email.toLowerCase().includes(q) || a.custom_label?.toLowerCase().includes(q));
    }, [accounts, searchQuery]);

    const filterCounts = useMemo(() => ({
        all: searchedAccounts.length,
        pro: searchedAccounts.filter(a => a.quota?.subscription_tier?.toLowerCase().includes('pro')).length,
        ultra: searchedAccounts.filter(a => a.quota?.subscription_tier?.toLowerCase().includes('ultra')).length,
        free: searchedAccounts.filter(a => {
            const tier = a.quota?.subscription_tier?.toLowerCase();
            return tier && !tier.includes('pro') && !tier.includes('ultra');
        }).length,
    }), [searchedAccounts]);

    const filteredAccounts = useMemo(() => {
        let result = searchedAccounts;
        if (filter === 'pro') result = result.filter(a => a.quota?.subscription_tier?.toLowerCase().includes('pro'));
        else if (filter === 'ultra') result = result.filter(a => a.quota?.subscription_tier?.toLowerCase().includes('ultra'));
        else if (filter === 'free') result = result.filter(a => {
            const tier = a.quota?.subscription_tier?.toLowerCase();
            return tier && !tier.includes('pro') && !tier.includes('ultra');
        });
        return result;
    }, [searchedAccounts, filter]);

    const paginatedAccounts = useMemo(() => {
        const start = (currentPage - 1) * ITEMS_PER_PAGE;
        return filteredAccounts.slice(start, start + ITEMS_PER_PAGE);
    }, [filteredAccounts, currentPage, ITEMS_PER_PAGE]);

    const totalPages = Math.ceil(filteredAccounts.length / ITEMS_PER_PAGE);

    // Handlers
    const handleToggleSelect = (id: string) => {
        const s = new Set(selectedIds);
        s.has(id) ? s.delete(id) : s.add(id);
        setSelectedIds(s);
    };

    const handleToggleAll = () => {
        const ids = paginatedAccounts.map(a => a.id);
        const allSelected = ids.every(id => selectedIds.has(id));
        const s = new Set(selectedIds);
        ids.forEach(id => allSelected ? s.delete(id) : s.add(id));
        setSelectedIds(s);
    };

    const handleSwitch = async (accountId: string) => {
        if (loading || switchingAccountId) return;
        setSwitchingAccountId(accountId);
        try {
            await switchAccount(accountId);
        } catch { /* handled by store */ }
        finally { setTimeout(() => setSwitchingAccountId(null), 500); }
    };

    const handleRefresh = async (accountId: string) => {
        setRefreshingIds(prev => new Set(prev).add(accountId));
        try { await refreshQuota(accountId); } catch { /* handled */ }
        finally { setRefreshingIds(prev => { const s = new Set(prev); s.delete(accountId); return s; }); }
    };

    const handleRefreshAll = async () => {
        setIsRefreshing(true);
        try {
            if (selectedIds.size > 0) {
                const ids = Array.from(selectedIds);
                setRefreshingIds(new Set(ids));
                await Promise.allSettled(ids.map(id => refreshQuota(id)));
            } else {
                setRefreshingIds(new Set(accounts.map(a => a.id)));
                await refreshAllQuotas();
            }
        } catch { /* handled */ }
        finally { setIsRefreshing(false); setRefreshingIds(new Set()); }
    };

    const handleWarmupAll = async () => {
        setIsWarmuping(true);
        try {
            if (selectedIds.size > 0) {
                await Promise.allSettled(Array.from(selectedIds).map(id => warmUpAccount(id)));
            } else {
                await warmUpAccounts();
            }
        } catch { /* handled */ }
        finally { setIsWarmuping(false); }
    };

    const handleDelete = async () => {
        if (isBatchDelete) {
            await deleteAccounts(Array.from(selectedIds));
            setSelectedIds(new Set());
        } else if (deleteConfirmId) {
            await deleteAccount(deleteConfirmId);
        }
        setDeleteConfirmId(null);
        setIsBatchDelete(false);
    };

    const handleExport = async () => {
        try {
            const ids = selectedIds.size > 0 ? Array.from(selectedIds) : accounts.map(a => a.id);
            const response = await exportAccounts(ids);
            if (!response.accounts?.length) return;
            const content = JSON.stringify(response.accounts, null, 2);
            const fileName = `kiro_accounts_${new Date().toISOString().split('T')[0]}.json`;
            const blob = new Blob([content], { type: 'application/json' });
            const url = URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url; a.download = fileName;
            document.body.appendChild(a); a.click();
            document.body.removeChild(a);
            URL.revokeObjectURL(url);
        } catch (error) { console.error('Export failed:', error); }
    };

    const handleImportJson = async () => {
        if (isTauri()) {
            try {
                const { open } = await import('@tauri-apps/plugin-dialog');
                const selected = await open({ multiple: false, filters: [{ name: 'JSON', extensions: ['json'] }] });
                if (!selected || typeof selected !== 'string') return;
                const content: string = await invoke('read_text_file', { path: selected });
                await processImportData(content);
            } catch (error) { console.error('Import failed:', error); }
        } else {
            fileInputRef.current?.click();
        }
    };

    const processImportData = async (content: string) => {
        try {
            const data = JSON.parse(content);
            if (!Array.isArray(data)) return;
            const valid = data.filter((item: { refresh_token?: string }) =>
                item.refresh_token && typeof item.refresh_token === 'string' && item.refresh_token.startsWith('1//')
            );
            for (const entry of valid) {
                try { await addAccount(entry.email || '', entry.refresh_token); } catch { /* skip */ }
                await new Promise(r => setTimeout(r, 100));
            }
        } catch { /* invalid JSON */ }
    };

    const handleFileChange = async (event: React.ChangeEvent<HTMLInputElement>) => {
        const file = event.target.files?.[0];
        if (!file) return;
        try { await processImportData(await file.text()); }
        finally { event.target.value = ''; }
    };

    const handleSaveLabel = async (accountId: string) => {
        try { await updateAccountLabel(accountId, editingLabelValue); }
        catch { /* handled */ }
        finally { setEditingLabelId(null); }
    };

    const getSubscriptionBadge = (tier?: string) => {
        if (!tier) return null;
        const t = tier.toLowerCase();
        if (t.includes('ultra')) return <span className="badge badge-xs badge-warning">Ultra</span>;
        if (t.includes('pro')) return <span className="badge badge-xs badge-info">Pro</span>;
        return <span className="badge badge-xs badge-ghost">Free</span>;
    };

    const getTopQuotas = (account: Account) => {
        if (!account.quota?.models?.length) return [];
        const models = showAllQuotas ? account.quota.models : account.quota.models.slice(0, 3);
        return models;
    };

    return (
        <div className="h-full flex flex-col p-5 gap-4 max-w-7xl mx-auto w-full">
            <input ref={fileInputRef} type="file" accept=".json" className="hidden" onChange={handleFileChange} />

            {/* Toolbar */}
            <div className="flex-none flex items-center gap-2 flex-wrap">
                {/* Search */}
                <div className="relative w-40">
                    <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-gray-400" />
                    <input
                        type="text"
                        placeholder={t('accounts.title')}
                        className="w-full pl-9 pr-3 py-2 text-sm border border-gray-200 dark:border-base-300 rounded-lg bg-white dark:bg-base-100 focus:outline-none focus:ring-2 focus:ring-blue-500"
                        value={searchQuery}
                        onChange={e => setSearchQuery(e.target.value)}
                    />
                </div>

                {/* View toggle */}
                <div className="flex gap-1 bg-gray-100 dark:bg-base-200 p-1 rounded-lg">
                    <button className={cn('p-1.5 rounded-md transition-all', viewMode === 'list' ? 'bg-white dark:bg-base-100 text-blue-600 shadow-sm' : 'text-gray-500')} onClick={() => setViewMode('list')}><List className="w-4 h-4" /></button>
                    <button className={cn('p-1.5 rounded-md transition-all', viewMode === 'grid' ? 'bg-white dark:bg-base-100 text-blue-600 shadow-sm' : 'text-gray-500')} onClick={() => setViewMode('grid')}><LayoutGrid className="w-4 h-4" /></button>
                </div>

                {/* Filter buttons */}
                <div className="flex gap-0.5 bg-gray-100 dark:bg-base-200 p-1 rounded-xl">
                    {(['all', 'pro', 'ultra', 'free'] as FilterType[]).map(f => (
                        <button key={f} className={cn('px-2 py-1.5 rounded-lg text-[11px] font-semibold flex items-center gap-1', filter === f ? 'bg-white dark:bg-base-100 text-blue-600 shadow-sm' : 'text-gray-500 hover:text-gray-900')} onClick={() => setFilter(f)}>
                            <span className="hidden md:inline capitalize">{f === 'all' ? t('common.all', 'All') : f.toUpperCase()}</span>
                            <span className={cn('px-1.5 py-0.5 rounded-md text-[10px] font-bold', filter === f ? 'bg-blue-100 dark:bg-blue-500/20 text-blue-600' : 'bg-gray-200 dark:bg-gray-700 text-gray-500')}>{filterCounts[f]}</span>
                        </button>
                    ))}
                </div>

                <div className="flex-1" />

                {/* Action buttons */}
                <div className="flex items-center gap-1.5">
                    {selectedIds.size > 0 && (
                        <>
                            <button className="px-2.5 py-2 bg-red-500 text-white text-xs rounded-lg hover:bg-red-600 flex items-center gap-1.5" onClick={() => setIsBatchDelete(true)}>
                                <Trash2 className="w-3.5 h-3.5" /><span className="hidden xl:inline">{t('common.delete')} ({selectedIds.size})</span>
                            </button>
                            <button className="px-2.5 py-2 bg-orange-500 text-white text-xs rounded-lg hover:bg-orange-600 flex items-center gap-1.5" onClick={() => Array.from(selectedIds).forEach(id => toggleProxyStatus(id, false))}>
                                <ToggleLeft className="w-3.5 h-3.5" />
                            </button>
                            <button className="px-2.5 py-2 bg-green-500 text-white text-xs rounded-lg hover:bg-green-600 flex items-center gap-1.5" onClick={() => Array.from(selectedIds).forEach(id => toggleProxyStatus(id, true))}>
                                <ToggleRight className="w-3.5 h-3.5" />
                            </button>
                        </>
                    )}
                    <button className={cn('px-2.5 py-2 bg-blue-500 text-white text-xs rounded-lg hover:bg-blue-600 flex items-center gap-1.5', isRefreshing && 'opacity-70')} onClick={handleRefreshAll} disabled={isRefreshing}>
                        <RefreshCw className={cn('w-3.5 h-3.5', isRefreshing && 'animate-spin')} /><span className="hidden xl:inline">{t('common.refresh')}</span>
                    </button>
                    <button className={cn('px-2.5 py-2 bg-orange-500 text-white text-xs rounded-lg hover:bg-orange-600 flex items-center gap-1.5', isWarmuping && 'opacity-70')} onClick={handleWarmupAll} disabled={isWarmuping}>
                        <Sparkles className={cn('w-3.5 h-3.5', isWarmuping && 'animate-pulse')} /><span className="hidden xl:inline">Warmup</span>
                    </button>
                    <label className="flex items-center gap-2 cursor-pointer px-2 py-2 hover:bg-gray-100 dark:hover:bg-base-200 rounded-lg">
                        <span className="text-xs text-gray-600 dark:text-gray-300 hidden xl:inline">Quotas</span>
                        <input type="checkbox" className="toggle toggle-xs toggle-primary" checked={showAllQuotas} onChange={toggleShowAllQuotas} />
                    </label>
                    <div className="w-px h-4 bg-gray-200 dark:bg-gray-700" />
                    <button className="px-2.5 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 text-xs rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 flex items-center gap-1.5" onClick={handleImportJson}>
                        <Upload className="w-3.5 h-3.5" /><span className="hidden lg:inline">{t('common.import')}</span>
                    </button>
                    <button className="px-2.5 py-2 border border-gray-200 dark:border-base-300 text-gray-700 dark:text-gray-300 text-xs rounded-lg hover:bg-gray-50 dark:hover:bg-base-200 flex items-center gap-1.5" onClick={handleExport}>
                        <Download className="w-3.5 h-3.5" /><span className="hidden lg:inline">{t('common.export')}</span>
                    </button>
                </div>
            </div>

            {/* Content */}
            <div className="flex-1 min-h-0">
                {viewMode === 'list' ? (
                    <div className="h-full bg-white dark:bg-base-100 rounded-2xl shadow-sm border border-gray-100 dark:border-base-200 flex flex-col overflow-hidden">
                        <div className="flex-1 overflow-y-auto">
                            <table className="table table-pin-rows w-full">
                                <thead>
                                    <tr className="bg-gray-50/50 dark:bg-base-200/50">
                                        <th className="bg-transparent w-10"><input type="checkbox" className="checkbox checkbox-xs" checked={paginatedAccounts.length > 0 && paginatedAccounts.every(a => selectedIds.has(a.id))} onChange={handleToggleAll} /></th>
                                        <th className="bg-transparent text-xs text-gray-500">Email</th>
                                        <th className="bg-transparent text-xs text-gray-500">Tier</th>
                                        <th className="bg-transparent text-xs text-gray-500">Quotas</th>
                                        <th className="bg-transparent text-xs text-gray-500">Status</th>
                                        <th className="bg-transparent text-xs text-gray-500 text-right">Actions</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {paginatedAccounts.map(account => (
                                        <tr key={account.id} className={cn('hover:bg-gray-50/80 dark:hover:bg-base-200/50 transition-colors group', currentAccount?.id === account.id && 'bg-blue-50/50 dark:bg-blue-900/10')}>
                                            <td><input type="checkbox" className="checkbox checkbox-xs" checked={selectedIds.has(account.id)} onChange={() => handleToggleSelect(account.id)} /></td>
                                            <td>
                                                <div className="flex items-center gap-2">
                                                    <div className="w-8 h-8 rounded-full bg-gradient-to-br from-blue-400 to-purple-500 flex items-center justify-center text-white text-xs font-bold">{account.email[0].toUpperCase()}</div>
                                                    <div>
                                                        <div className="text-sm font-medium text-gray-900 dark:text-base-content">{account.email}</div>
                                                        {editingLabelId === account.id ? (
                                                            <input className="input input-xs input-bordered w-32" value={editingLabelValue} onChange={e => setEditingLabelValue(e.target.value)} onBlur={() => handleSaveLabel(account.id)} onKeyDown={e => e.key === 'Enter' && handleSaveLabel(account.id)} autoFocus />
                                                        ) : (
                                                            <div className="text-[10px] text-gray-400 cursor-pointer hover:text-blue-500" onClick={() => { setEditingLabelId(account.id); setEditingLabelValue(account.custom_label || ''); }}>
                                                                {account.custom_label || <Tag className="w-3 h-3 inline" />}
                                                            </div>
                                                        )}
                                                    </div>
                                                </div>
                                            </td>
                                            <td>{getSubscriptionBadge(account.quota?.subscription_tier)}</td>
                                            <td>
                                                <div className="flex flex-wrap gap-1">
                                                    {getTopQuotas(account).map(q => (
                                                        <div key={q.name} className="flex items-center gap-1">
                                                            <div className={cn('w-1.5 h-1.5 rounded-full', q.percentage >= 50 ? 'bg-green-500' : q.percentage >= 20 ? 'bg-yellow-500' : 'bg-red-500')} />
                                                            <span className="text-[10px] text-gray-500">{q.name.replace('gemini-', 'g-').replace('claude-', 'c-')}</span>
                                                            <span className="text-[10px] font-mono font-medium">{q.percentage}%</span>
                                                        </div>
                                                    ))}
                                                    {account.quota?.is_forbidden && <span className="badge badge-xs badge-error">403</span>}
                                                </div>
                                            </td>
                                            <td>
                                                <div className="flex items-center gap-1">
                                                    {account.proxy_disabled && <span className="badge badge-xs badge-warning">Proxy Off</span>}
                                                    {account.validation_blocked && <span className="badge badge-xs badge-error">Blocked</span>}
                                                    {account.disabled && <span className="badge badge-xs badge-ghost">Disabled</span>}
                                                    {!account.proxy_disabled && !account.validation_blocked && !account.disabled && <span className="badge badge-xs badge-success">Active</span>}
                                                </div>
                                            </td>
                                            <td>
                                                <div className="flex justify-end gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
                                                    {currentAccount?.id !== account.id && (
                                                        <button className="btn btn-xs btn-ghost text-blue-500" onClick={() => handleSwitch(account.id)} disabled={!!switchingAccountId}>
                                                            {switchingAccountId === account.id ? <RefreshCw className="w-3 h-3 animate-spin" /> : 'Switch'}
                                                        </button>
                                                    )}
                                                    <button className={cn('btn btn-xs btn-ghost', refreshingIds.has(account.id) && 'text-blue-500')} onClick={() => handleRefresh(account.id)}>
                                                        <RefreshCw className={cn('w-3 h-3', refreshingIds.has(account.id) && 'animate-spin')} />
                                                    </button>
                                                    <button className="btn btn-xs btn-ghost text-gray-500 hover:text-red-500" onClick={() => setDeleteConfirmId(account.id)}>
                                                        <Trash2 className="w-3 h-3" />
                                                    </button>
                                                </div>
                                            </td>
                                        </tr>
                                    ))}
                                    {paginatedAccounts.length === 0 && (
                                        <tr><td colSpan={6} className="text-center py-12 text-gray-400">{t('accounts.list.empty')}</td></tr>
                                    )}
                                </tbody>
                            </table>
                        </div>
                    </div>
                ) : (
                    <div className="h-full overflow-y-auto">
                        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-4">
                            {paginatedAccounts.map(account => (
                                <div key={account.id} className={cn('bg-white dark:bg-base-100 rounded-xl shadow-sm border border-gray-100 dark:border-base-200 p-4 hover:shadow-md transition-all cursor-pointer group', currentAccount?.id === account.id && 'ring-2 ring-blue-500', selectedIds.has(account.id) && 'ring-2 ring-purple-500')}>
                                    <div className="flex items-start justify-between mb-3">
                                        <div className="flex items-center gap-2">
                                            <input type="checkbox" className="checkbox checkbox-xs" checked={selectedIds.has(account.id)} onChange={() => handleToggleSelect(account.id)} />
                                            <div className="w-8 h-8 rounded-full bg-gradient-to-br from-blue-400 to-purple-500 flex items-center justify-center text-white text-xs font-bold">{account.email[0].toUpperCase()}</div>
                                        </div>
                                        {getSubscriptionBadge(account.quota?.subscription_tier)}
                                    </div>
                                    <div className="text-sm font-medium text-gray-900 dark:text-base-content truncate mb-1">{account.email}</div>
                                    {account.custom_label && <div className="text-[10px] text-gray-400 mb-2">{account.custom_label}</div>}
                                    <div className="space-y-1 mb-3">
                                        {getTopQuotas(account).map(q => (
                                            <div key={q.name} className="flex items-center gap-2">
                                                <span className="text-[10px] text-gray-500 w-20 truncate">{q.name.replace('gemini-', 'g-').replace('claude-', 'c-')}</span>
                                                <div className="flex-1 h-1.5 bg-gray-100 dark:bg-base-200 rounded-full overflow-hidden">
                                                    <div className={cn('h-full rounded-full', q.percentage >= 50 ? 'bg-green-500' : q.percentage >= 20 ? 'bg-yellow-500' : 'bg-red-500')} style={{ width: `${q.percentage}%` }} />
                                                </div>
                                                <span className="text-[10px] font-mono w-8 text-right">{q.percentage}%</span>
                                            </div>
                                        ))}
                                    </div>
                                    <div className="flex items-center justify-between">
                                        <div className="flex gap-1">
                                            {account.proxy_disabled && <span className="badge badge-xs badge-warning">Off</span>}
                                            {account.validation_blocked && <span className="badge badge-xs badge-error">Blocked</span>}
                                            {!account.proxy_disabled && !account.validation_blocked && <span className="badge badge-xs badge-success">Active</span>}
                                        </div>
                                        <div className="flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
                                            <button className="btn btn-xs btn-ghost" onClick={() => handleRefresh(account.id)}><RefreshCw className={cn('w-3 h-3', refreshingIds.has(account.id) && 'animate-spin')} /></button>
                                            <button className="btn btn-xs btn-ghost text-red-500" onClick={() => setDeleteConfirmId(account.id)}><Trash2 className="w-3 h-3" /></button>
                                        </div>
                                    </div>
                                </div>
                            ))}
                        </div>
                    </div>
                )}
            </div>

            {/* Pagination */}
            {totalPages > 1 && (
                <div className="flex-none flex items-center justify-center gap-2">
                    <button className="btn btn-xs btn-ghost" disabled={currentPage <= 1} onClick={() => setCurrentPage(p => p - 1)}>{t('common.prev_page')}</button>
                    <span className="text-xs text-gray-500">{currentPage} / {totalPages}</span>
                    <button className="btn btn-xs btn-ghost" disabled={currentPage >= totalPages} onClick={() => setCurrentPage(p => p + 1)}>{t('common.next_page')}</button>
                </div>
            )}

            {/* Delete confirmation modal */}
            {(deleteConfirmId || isBatchDelete) && (
                <div className="modal modal-open">
                    <div className="modal-box">
                        <h3 className="font-bold text-lg">{t('common.confirm')}</h3>
                        <p className="py-4">{isBatchDelete ? `Delete ${selectedIds.size} accounts?` : 'Delete this account?'}</p>
                        <div className="modal-action">
                            <button className="btn btn-ghost" onClick={() => { setDeleteConfirmId(null); setIsBatchDelete(false); }}>{t('common.cancel')}</button>
                            <button className="btn btn-error" onClick={handleDelete}>{t('common.delete')}</button>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
}

export default Accounts;
