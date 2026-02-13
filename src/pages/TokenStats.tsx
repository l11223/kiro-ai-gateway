import { useEffect, useState } from 'react';
import { request as invoke } from '../utils/request';
import { useTranslation } from 'react-i18next';
import { AreaChart, Area, BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, Legend } from 'recharts';
import { Clock, Calendar, CalendarDays, Users, Zap, TrendingUp, RefreshCw, Cpu } from 'lucide-react';
import { cn } from '../utils/cn';

interface TokenStatsAggregated { period: string; total_input_tokens: number; total_output_tokens: number; total_tokens: number; request_count: number; }
interface ModelTokenStats { model: string; total_input_tokens: number; total_output_tokens: number; total_tokens: number; request_count: number; }
interface ModelTrendPoint { period: string; model_data: Record<string, number>; }
interface AccountTrendPoint { period: string; account_data: Record<string, number>; }
interface TokenStatsSummary { total_input_tokens: number; total_output_tokens: number; total_tokens: number; total_requests: number; unique_accounts: number; }

type TimeRange = 'hourly' | 'daily' | 'weekly';
type ViewMode = 'model' | 'account';

const COLORS = ['#3b82f6', '#8b5cf6', '#ec4899', '#f59e0b', '#10b981', '#06b6d4', '#6366f1', '#f43f5e', '#84cc16', '#a855f7', '#14b8a6', '#f97316'];
const formatNumber = (num: number): string => { if (num >= 1e6) return `${(num / 1e6).toFixed(1)}M`; if (num >= 1e3) return `${(num / 1e3).toFixed(1)}K`; return num.toString(); };

const TokenStats: React.FC = () => {
    const { t } = useTranslation();
    const [timeRange, setTimeRange] = useState<TimeRange>('daily');
    const [viewMode, setViewMode] = useState<ViewMode>('model');
    const [chartData, setChartData] = useState<TokenStatsAggregated[]>([]);
    const [modelData, setModelData] = useState<ModelTokenStats[]>([]);
    const [trendData, setTrendData] = useState<Record<string, unknown>[]>([]);
    const [allKeys, setAllKeys] = useState<string[]>([]);
    const [summary, setSummary] = useState<TokenStatsSummary | null>(null);
    const [loading, setLoading] = useState(true);

    const fetchData = async () => {
        setLoading(true);
        try {
            let hours = 24;
            let data: TokenStatsAggregated[] = [];
            let modelTrend: ModelTrendPoint[] = [];
            let accountTrend: AccountTrendPoint[] = [];

            switch (timeRange) {
                case 'hourly': hours = 24;
                    data = await invoke<TokenStatsAggregated[]>('get_token_stats_hourly', { hours: 24 });
                    modelTrend = await invoke<ModelTrendPoint[]>('get_token_stats_model_trend_hourly', { hours: 24 });
                    accountTrend = await invoke<AccountTrendPoint[]>('get_token_stats_account_trend_hourly', { hours: 24 });
                    break;
                case 'daily': hours = 168;
                    data = await invoke<TokenStatsAggregated[]>('get_token_stats_daily', { days: 7 });
                    modelTrend = await invoke<ModelTrendPoint[]>('get_token_stats_model_trend_daily', { days: 7 });
                    accountTrend = await invoke<AccountTrendPoint[]>('get_token_stats_account_trend_daily', { days: 7 });
                    break;
                case 'weekly': hours = 720;
                    data = await invoke<TokenStatsAggregated[]>('get_token_stats_weekly', { weeks: 4 });
                    modelTrend = await invoke<ModelTrendPoint[]>('get_token_stats_model_trend_daily', { days: 30 });
                    accountTrend = await invoke<AccountTrendPoint[]>('get_token_stats_account_trend_daily', { days: 30 });
                    break;
            }
            setChartData(data);

            // Process trend data based on view mode
            const source = viewMode === 'model' ? modelTrend : accountTrend;
            const keysSet = new Set<string>();
            source.forEach((point: ModelTrendPoint | AccountTrendPoint) => {
                const d = viewMode === 'model' ? (point as ModelTrendPoint).model_data : (point as AccountTrendPoint).account_data;
                Object.keys(d).forEach(k => keysSet.add(k));
            });
            const keys = Array.from(keysSet);
            setAllKeys(keys);
            const transformed = source.map((point: ModelTrendPoint | AccountTrendPoint) => {
                const d = viewMode === 'model' ? (point as ModelTrendPoint).model_data : (point as AccountTrendPoint).account_data;
                const row: Record<string, unknown> = { period: point.period };
                keys.forEach(k => { row[k] = d[k] || 0; });
                return row;
            });
            setTrendData(transformed);

            const [models, summaryData] = await Promise.all([
                invoke<ModelTokenStats[]>('get_token_stats_by_model', { hours }),
                invoke<TokenStatsSummary>('get_token_stats_summary', { hours }),
            ]);
            setModelData(models);
            setSummary(summaryData);
        } catch (error) { console.error('Failed to fetch token stats:', error); }
        finally { setLoading(false); }
    };

    useEffect(() => { fetchData(); }, [timeRange, viewMode]);

    const summaryCards = summary ? [
        { icon: Zap, label: t('token_stats.total_tokens', 'Total Tokens'), value: formatNumber(summary.total_tokens), color: 'gray' },
        { icon: TrendingUp, label: t('token_stats.input_tokens', 'Input'), value: formatNumber(summary.total_input_tokens), color: 'blue' },
        { icon: TrendingUp, label: t('token_stats.output_tokens', 'Output'), value: formatNumber(summary.total_output_tokens), color: 'purple' },
        { icon: Users, label: t('token_stats.accounts_used', 'Accounts'), value: summary.unique_accounts, color: 'green' },
        { icon: Cpu, label: t('token_stats.models_used', 'Models'), value: modelData.length, color: 'orange' },
    ] : [];

    return (
        <div className="h-full w-full overflow-y-auto">
            <div className="p-5 space-y-4 max-w-7xl mx-auto">
                <div className="flex items-center justify-between">
                    <h1 className="text-2xl font-bold text-gray-800 dark:text-white flex items-center gap-2">
                        <Zap className="w-6 h-6 text-blue-500" />{t('token_stats.title')}
                    </h1>
                    <div className="flex items-center gap-2">
                        <div className="flex bg-gray-100 dark:bg-gray-800 rounded-lg p-1">
                            {([{ key: 'hourly', icon: Clock }, { key: 'daily', icon: Calendar }, { key: 'weekly', icon: CalendarDays }] as const).map(({ key, icon: Icon }) => (
                                <button key={key} onClick={() => setTimeRange(key)} className={cn('px-3 py-1.5 rounded-md text-sm font-medium flex items-center gap-1.5', timeRange === key ? 'bg-white dark:bg-gray-700 text-blue-600 shadow-sm' : 'text-gray-600 dark:text-gray-400')}>
                                    <Icon className="w-4 h-4" />{key.charAt(0).toUpperCase() + key.slice(1)}
                                </button>
                            ))}
                        </div>
                        <button onClick={fetchData} disabled={loading} className="p-2 rounded-lg bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50">
                            <RefreshCw className={cn('w-4 h-4', loading && 'animate-spin')} />
                        </button>
                    </div>
                </div>

                {/* Summary cards */}
                {summary && (
                    <div className="grid grid-cols-2 md:grid-cols-5 gap-4">
                        {summaryCards.map((card, i) => (
                            <div key={i} className="bg-white dark:bg-gray-800 rounded-xl p-4 shadow-sm border border-gray-200 dark:border-gray-700">
                                <div className="flex items-center gap-2 text-gray-500 text-sm mb-2">
                                    <card.icon className="w-4 h-4" />{card.label}
                                </div>
                                <div className="text-2xl font-bold text-gray-800 dark:text-white">{card.value}</div>
                            </div>
                        ))}
                    </div>
                )}

                {/* Trend chart */}
                <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-sm border border-gray-200 dark:border-gray-700">
                    <div className="flex items-center justify-between mb-4">
                        <h2 className="text-lg font-semibold text-gray-800 dark:text-white flex items-center gap-2">
                            {viewMode === 'model' ? <Cpu className="w-5 h-5 text-purple-500" /> : <Users className="w-5 h-5 text-green-500" />}
                            {viewMode === 'model' ? t('token_stats.model_trend', 'Model Trend') : t('token_stats.account_trend', 'Account Trend')}
                        </h2>
                        <div className="flex bg-gray-100 dark:bg-gray-700 rounded-lg p-1">
                            <button onClick={() => setViewMode('model')} className={cn('px-3 py-1 text-xs font-medium rounded-md', viewMode === 'model' ? 'bg-white dark:bg-gray-600 text-blue-600 shadow-sm' : 'text-gray-500')}>{t('token_stats.by_model', 'By Model')}</button>
                            <button onClick={() => setViewMode('account')} className={cn('px-3 py-1 text-xs font-medium rounded-md', viewMode === 'account' ? 'bg-white dark:bg-gray-600 text-blue-600 shadow-sm' : 'text-gray-500')}>{t('token_stats.by_account', 'By Account')}</button>
                        </div>
                    </div>
                    <div className="h-72">
                        {trendData.length > 0 && allKeys.length > 0 ? (
                            <ResponsiveContainer width="100%" height="100%">
                                <AreaChart data={trendData}>
                                    <CartesianGrid strokeDasharray="3 3" vertical={false} stroke="#374151" strokeOpacity={0.15} />
                                    <XAxis dataKey="period" tick={{ fontSize: 11, fill: '#6b7280' }} axisLine={false} tickLine={false} />
                                    <YAxis tick={{ fontSize: 11, fill: '#6b7280' }} tickFormatter={formatNumber} axisLine={false} tickLine={false} />
                                    <Tooltip />
                                    <Legend wrapperStyle={{ fontSize: '11px', paddingTop: '10px' }} />
                                    {allKeys.map((key, i) => (
                                        <Area key={key} type="monotone" dataKey={key} stackId="1" stroke={COLORS[i % COLORS.length]} fill={COLORS[i % COLORS.length]} fillOpacity={0.6} />
                                    ))}
                                </AreaChart>
                            </ResponsiveContainer>
                        ) : (
                            <div className="h-full flex items-center justify-center text-gray-400">{loading ? t('common.loading') : t('common.empty')}</div>
                        )}
                    </div>
                </div>

                {/* Bar chart */}
                <div className="bg-white dark:bg-gray-800 rounded-xl p-6 shadow-sm border border-gray-200 dark:border-gray-700">
                    <h2 className="text-lg font-semibold text-gray-800 dark:text-white mb-4">{t('token_stats.usage_trend', 'Usage Trend')}</h2>
                    <div className="h-64">
                        {chartData.length > 0 ? (
                            <ResponsiveContainer width="100%" height="100%">
                                <BarChart data={chartData}>
                                    <CartesianGrid strokeDasharray="3 3" vertical={false} stroke="#374151" strokeOpacity={0.15} />
                                    <XAxis dataKey="period" tick={{ fontSize: 11, fill: '#6b7280' }} axisLine={false} tickLine={false} />
                                    <YAxis tick={{ fontSize: 11, fill: '#6b7280' }} tickFormatter={formatNumber} axisLine={false} tickLine={false} />
                                    <Tooltip />
                                    <Bar dataKey="total_input_tokens" name="Input" fill="#3b82f6" radius={[4, 4, 0, 0]} />
                                    <Bar dataKey="total_output_tokens" name="Output" fill="#8b5cf6" radius={[4, 4, 0, 0]} />
                                </BarChart>
                            </ResponsiveContainer>
                        ) : (
                            <div className="h-full flex items-center justify-center text-gray-400">{loading ? t('common.loading') : t('common.empty')}</div>
                        )}
                    </div>
                </div>
            </div>
        </div>
    );
};

export default TokenStats;
