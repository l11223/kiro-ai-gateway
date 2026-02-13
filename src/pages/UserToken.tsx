import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Plus, Trash2, RefreshCw, Copy, Activity, User, Settings, Shield, Clock, Users } from 'lucide-react';
import { request as invoke } from '../utils/request';
import { copyToClipboard } from '../utils/clipboard';
import { cn } from '../utils/cn';

interface UserTokenItem {
    id: string; token: string; username: string; description?: string; enabled: boolean;
    expires_type: string; expires_at?: number; max_ips: number;
    curfew_start?: string; curfew_end?: string;
    created_at: number; updated_at: number; last_used_at?: number;
    total_requests: number; total_tokens_used: number;
}

interface UserTokenStats { total_tokens: number; active_tokens: number; total_users: number; today_requests: number; }

const UserToken: React.FC = () => {
    const { t } = useTranslation();
    const [tokens, setTokens] = useState<UserTokenItem[]>([]);
    const [stats, setStats] = useState<UserTokenStats | null>(null);
    const [loading, setLoading] = useState(false);
    const [showCreateModal, setShowCreateModal] = useState(false);
    const [showEditModal, setShowEditModal] = useState(false);
    const [editingToken, setEditingToken] = useState<UserTokenItem | null>(null);

    // Create form
    const [newUsername, setNewUsername] = useState('');
    const [newDesc, setNewDesc] = useState('');
    const [newExpiresType, setNewExpiresType] = useState('month');
    const [newMaxIps, setNewMaxIps] = useState(0);
    const [newCurfewStart, setNewCurfewStart] = useState('');
    const [newCurfewEnd, setNewCurfewEnd] = useState('');

    // Edit form
    const [editUsername, setEditUsername] = useState('');
    const [editDesc, setEditDesc] = useState('');
    const [editMaxIps, setEditMaxIps] = useState(0);
    const [editCurfewStart, setEditCurfewStart] = useState('');
    const [editCurfewEnd, setEditCurfewEnd] = useState('');

    const loadData = async () => {
        setLoading(true);
        try {
            const [tokensData, statsData] = await Promise.all([
                invoke<UserTokenItem[]>('list_user_tokens'),
                invoke<UserTokenStats>('get_user_token_summary'),
            ]);
            setTokens(tokensData); setStats(statsData);
        } catch { /* handled */ }
        finally { setLoading(false); }
    };

    useEffect(() => { loadData(); }, []);

    const handleCreate = async () => {
        if (!newUsername) return;
        try {
            await invoke('create_user_token', { request: { username: newUsername, expires_type: newExpiresType, description: newDesc || null, max_ips: newMaxIps, curfew_start: newCurfewStart || null, curfew_end: newCurfewEnd || null, custom_expires_at: null } });
            setShowCreateModal(false);
            setNewUsername(''); setNewDesc(''); setNewExpiresType('month'); setNewMaxIps(0); setNewCurfewStart(''); setNewCurfewEnd('');
            loadData();
        } catch { /* handled */ }
    };

    const handleEdit = (token: UserTokenItem) => {
        setEditingToken(token); setEditUsername(token.username); setEditDesc(token.description || '');
        setEditMaxIps(token.max_ips ?? 0); setEditCurfewStart(token.curfew_start ?? ''); setEditCurfewEnd(token.curfew_end ?? '');
        setShowEditModal(true);
    };

    const handleUpdate = async () => {
        if (!editingToken || !editUsername) return;
        try {
            await invoke('update_user_token', { id: editingToken.id, request: { username: editUsername, description: editDesc || undefined, max_ips: editMaxIps, curfew_start: editCurfewStart || null, curfew_end: editCurfewEnd || null } });
            setShowEditModal(false); setEditingToken(null); loadData();
        } catch { /* handled */ }
    };

    const handleDelete = async (id: string) => { try { await invoke('delete_user_token', { id }); loadData(); } catch { /* */ } };
    const handleRenew = async (id: string, type: string) => { try { await invoke('renew_user_token', { id, expiresType: type }); loadData(); } catch { /* */ } };
    const handleCopy = async (text: string) => { await copyToClipboard(text); };
    const formatTime = (ts?: number) => ts ? new Date(ts * 1000).toLocaleString() : '-';

    const getExpiresLabel = (type: string) => {
        const map: Record<string, string> = { day: '1 Day', week: '1 Week', month: '1 Month', never: 'Never', custom: 'Custom' };
        return map[type] || type;
    };

    const getExpiresStatus = (expiresAt?: number) => {
        if (!expiresAt) return 'text-green-500';
        const now = Date.now() / 1000;
        if (expiresAt < now) return 'text-red-500 font-bold';
        if (expiresAt - now < 86400 * 3) return 'text-orange-500';
        return 'text-green-500';
    };

    return (
        <div className="h-full flex flex-col p-5 gap-5 max-w-7xl mx-auto w-full">
            {/* Header */}
            <div className="flex justify-between items-center">
                <h1 className="text-2xl font-bold text-gray-900 dark:text-white flex items-center gap-2">
                    <div className="p-2 bg-purple-50 dark:bg-purple-900/20 rounded-lg"><User className="text-purple-500 w-5 h-5" /></div>
                    {t('user_token.title')}
                </h1>
                <div className="flex items-center gap-2">
                    <button onClick={loadData} className={cn('p-2 hover:bg-gray-100 dark:hover:bg-base-200 rounded-lg', loading && 'text-blue-500')}>
                        <RefreshCw size={18} className={loading ? 'animate-spin' : ''} />
                    </button>
                    <button onClick={() => setShowCreateModal(true)} className="px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white text-sm font-medium rounded-lg flex items-center gap-2 shadow-sm">
                        <Plus size={16} />{t('user_token.create', { defaultValue: 'Create Token' })}
                    </button>
                </div>
            </div>

            {/* Stats */}
            <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
                {[
                    { icon: Users, color: 'blue', value: stats?.total_users || 0, label: 'Total Users' },
                    { icon: Activity, color: 'green', value: stats?.active_tokens || 0, label: 'Active Tokens' },
                    { icon: Clock, color: 'purple', value: stats?.total_tokens || 0, label: 'Total Tokens' },
                    { icon: Shield, color: 'orange', value: stats?.today_requests || 0, label: 'Today Requests' },
                ].map((card, i) => (
                    <div key={i} className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200">
                        <div className="flex items-center justify-between mb-2">
                            <div className={`p-1.5 bg-${card.color}-50 dark:bg-${card.color}-900/20 rounded-md`}><card.icon className={`w-4 h-4 text-${card.color}-500`} /></div>
                        </div>
                        <div className="text-2xl font-bold text-gray-900 dark:text-base-content mb-0.5">{card.value}</div>
                        <div className="text-xs text-gray-500">{card.label}</div>
                    </div>
                ))}
            </div>

            {/* Token list */}
            <div className="flex-1 overflow-auto bg-white dark:bg-base-100 rounded-2xl shadow-sm border border-gray-100 dark:border-base-200">
                <table className="table table-pin-rows">
                    <thead><tr className="bg-gray-50/50 dark:bg-base-200/50">
                        <th className="bg-transparent text-gray-500 font-medium py-4">Username</th>
                        <th className="bg-transparent text-gray-500 font-medium py-4">Token</th>
                        <th className="bg-transparent text-gray-500 font-medium py-4">Expires</th>
                        <th className="bg-transparent text-gray-500 font-medium py-4">Usage</th>
                        <th className="bg-transparent text-gray-500 font-medium py-4">IP Limit</th>
                        <th className="bg-transparent text-gray-500 font-medium py-4">Created</th>
                        <th className="bg-transparent text-gray-500 font-medium py-4 text-right">Actions</th>
                    </tr></thead>
                    <tbody className="divide-y divide-gray-50 dark:divide-base-200">
                        {tokens.map(token => (
                            <tr key={token.id} className="hover:bg-gray-50/80 dark:hover:bg-base-200/50 group">
                                <td className="py-4">
                                    <div className="flex items-center gap-3">
                                        <div className="w-8 h-8 rounded-full bg-purple-50 dark:bg-purple-900/20 flex items-center justify-center text-purple-600 font-bold text-xs">{token.username.substring(0, 2).toUpperCase()}</div>
                                        <div>
                                            <div className="font-semibold text-gray-900 dark:text-white text-xs uppercase tracking-wider">{token.username}</div>
                                            <div className="text-[10px] text-gray-500">{token.description || '-'}</div>
                                        </div>
                                    </div>
                                </td>
                                <td>
                                    <div className="flex items-center gap-2">
                                        <code className="bg-gray-50 dark:bg-base-200 px-2 py-1 rounded text-[11px] font-mono text-gray-600 dark:text-gray-400">{token.token.substring(0, 8)}••••</code>
                                        <button onClick={() => handleCopy(token.token)} className="p-1.5 hover:bg-gray-200 dark:hover:bg-base-300 rounded-md text-gray-400 hover:text-gray-600"><Copy size={13} /></button>
                                    </div>
                                </td>
                                <td>
                                    <div className={cn('text-xs font-medium mb-1', getExpiresStatus(token.expires_at))}>{token.expires_at ? formatTime(token.expires_at) : 'Never'}</div>
                                    <span className="text-[10px] px-1.5 py-0.5 bg-gray-100 dark:bg-base-200 text-gray-500 rounded">{getExpiresLabel(token.expires_type)}</span>
                                </td>
                                <td>
                                    <div className="text-xs font-semibold text-gray-700 dark:text-gray-300">{token.total_requests} <span className="text-[10px] font-normal text-gray-400">reqs</span></div>
                                    <div className="text-[10px] text-gray-400">{(token.total_tokens_used / 1000).toFixed(1)}k tokens</div>
                                </td>
                                <td>
                                    {token.max_ips === 0 ? <span className="badge badge-xs badge-ghost">Unlimited</span> : <span className="badge badge-xs badge-warning">{token.max_ips} IPs</span>}
                                    {token.curfew_start && token.curfew_end && <div className="text-[10px] text-gray-400 mt-1 flex items-center gap-1"><Clock size={10} className="text-orange-500" />{token.curfew_start}-{token.curfew_end}</div>}
                                </td>
                                <td className="text-[10px] text-gray-400">{formatTime(token.created_at)}</td>
                                <td className="text-right">
                                    <div className="flex justify-end gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
                                        <button onClick={() => handleEdit(token)} className="p-1.5 hover:bg-gray-100 dark:hover:bg-base-200 rounded-lg text-gray-500 hover:text-blue-500"><Settings size={14} /></button>
                                        <button onClick={() => handleRenew(token.id, token.expires_type)} className="p-1.5 hover:bg-gray-100 dark:hover:bg-base-200 rounded-lg text-gray-500 hover:text-green-500"><RefreshCw size={14} /></button>
                                        <button onClick={() => handleDelete(token.id)} className="p-1.5 hover:bg-red-50 dark:hover:bg-red-900/20 rounded-lg text-gray-400 hover:text-red-500"><Trash2 size={14} /></button>
                                    </div>
                                </td>
                            </tr>
                        ))}
                        {tokens.length === 0 && !loading && (
                            <tr><td colSpan={7} className="py-20 text-center text-gray-400">
                                <Users size={40} className="mx-auto mb-3 opacity-20" />
                                <p className="text-sm">{t('common.empty')}</p>
                            </td></tr>
                        )}
                    </tbody>
                </table>
            </div>

            {/* Create Modal */}
            {showCreateModal && (
                <div className="modal modal-open">
                    <div className="modal-box">
                        <h3 className="font-bold text-lg mb-4">Create New Token</h3>
                        <div className="space-y-3">
                            <div><label className="label"><span className="label-text">Username *</span></label><input type="text" className="input input-bordered w-full" value={newUsername} onChange={e => setNewUsername(e.target.value)} /></div>
                            <div><label className="label"><span className="label-text">Description</span></label><input type="text" className="input input-bordered w-full" value={newDesc} onChange={e => setNewDesc(e.target.value)} /></div>
                            <div className="grid grid-cols-2 gap-4">
                                <div><label className="label"><span className="label-text">Expires</span></label><select className="select select-bordered w-full" value={newExpiresType} onChange={e => setNewExpiresType(e.target.value)}><option value="day">1 Day</option><option value="week">1 Week</option><option value="month">1 Month</option><option value="never">Never</option></select></div>
                                <div><label className="label"><span className="label-text">Max IPs</span></label><input type="number" className="input input-bordered w-full" value={newMaxIps} onChange={e => setNewMaxIps(parseInt(e.target.value) || 0)} min="0" /></div>
                            </div>
                            <div className="grid grid-cols-2 gap-4">
                                <div><label className="label"><span className="label-text">Curfew Start</span></label><input type="time" className="input input-bordered w-full" value={newCurfewStart} onChange={e => setNewCurfewStart(e.target.value)} /></div>
                                <div><label className="label"><span className="label-text">Curfew End</span></label><input type="time" className="input input-bordered w-full" value={newCurfewEnd} onChange={e => setNewCurfewEnd(e.target.value)} /></div>
                            </div>
                        </div>
                        <div className="modal-action">
                            <button className="btn btn-ghost" onClick={() => setShowCreateModal(false)}>Cancel</button>
                            <button className="btn btn-primary" onClick={handleCreate}>Create</button>
                        </div>
                    </div>
                </div>
            )}

            {/* Edit Modal */}
            {showEditModal && editingToken && (
                <div className="modal modal-open">
                    <div className="modal-box">
                        <h3 className="font-bold text-lg mb-4">Edit Token</h3>
                        <div className="space-y-3">
                            <div><label className="label"><span className="label-text">Username *</span></label><input type="text" className="input input-bordered w-full" value={editUsername} onChange={e => setEditUsername(e.target.value)} /></div>
                            <div><label className="label"><span className="label-text">Description</span></label><input type="text" className="input input-bordered w-full" value={editDesc} onChange={e => setEditDesc(e.target.value)} /></div>
                            <div><label className="label"><span className="label-text">Max IPs</span></label><input type="number" className="input input-bordered w-full" value={editMaxIps} onChange={e => setEditMaxIps(parseInt(e.target.value) || 0)} min="0" /></div>
                            <div className="grid grid-cols-2 gap-4">
                                <div><label className="label"><span className="label-text">Curfew Start</span></label><input type="time" className="input input-bordered w-full" value={editCurfewStart} onChange={e => setEditCurfewStart(e.target.value)} /></div>
                                <div><label className="label"><span className="label-text">Curfew End</span></label><input type="time" className="input input-bordered w-full" value={editCurfewEnd} onChange={e => setEditCurfewEnd(e.target.value)} /></div>
                            </div>
                        </div>
                        <div className="modal-action">
                            <button className="btn btn-ghost" onClick={() => setShowEditModal(false)}>Cancel</button>
                            <button className="btn btn-primary" onClick={handleUpdate}>Update</button>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
};

export default UserToken;
