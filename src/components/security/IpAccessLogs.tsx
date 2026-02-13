import { useTranslation } from 'react-i18next';
import Pagination from '../common/Pagination';

interface IpLog {
    id: string;
    client_ip: string;
    timestamp: number;
    method?: string;
    path?: string;
    status?: number;
    blocked?: boolean;
    block_reason?: string;
}

interface IpAccessLogsProps {
    logs: IpLog[];
    currentPage: number;
    totalPages: number;
    totalItems: number;
    onPageChange: (page: number) => void;
}

export default function IpAccessLogs({ logs, currentPage, totalPages, totalItems, onPageChange }: IpAccessLogsProps) {
    const { t } = useTranslation();

    return (
        <div className="bg-white dark:bg-base-100 rounded-xl shadow-sm border border-gray-100 dark:border-base-200 overflow-hidden">
            <div className="overflow-x-auto">
                <table className="table table-xs w-full">
                    <thead>
                        <tr className="bg-gray-50 dark:bg-base-200">
                            <th>IP</th>
                            <th>{t('security.method', 'Method')}</th>
                            <th>{t('security.path', 'Path')}</th>
                            <th>{t('security.status', 'Status')}</th>
                            <th>{t('security.time', 'Time')}</th>
                            <th>{t('security.blocked', 'Blocked')}</th>
                        </tr>
                    </thead>
                    <tbody>
                        {logs.map(log => (
                            <tr key={log.id} className="hover:bg-gray-50 dark:hover:bg-base-200">
                                <td className="font-mono text-xs">{log.client_ip}</td>
                                <td className="text-xs">{log.method}</td>
                                <td className="text-xs truncate max-w-[200px]" title={log.path}>{log.path}</td>
                                <td><span className={`badge badge-xs ${log.status && log.status < 400 ? 'badge-success' : 'badge-error'}`}>{log.status}</span></td>
                                <td className="text-xs">{new Date(log.timestamp * 1000).toLocaleString()}</td>
                                <td>{log.blocked && <span className="badge badge-xs badge-error">Blocked</span>}</td>
                            </tr>
                        ))}
                        {logs.length === 0 && (
                            <tr><td colSpan={6} className="text-center py-8 text-gray-400">{t('common.empty')}</td></tr>
                        )}
                    </tbody>
                </table>
            </div>
            <Pagination currentPage={currentPage} totalPages={totalPages} totalItems={totalItems} itemsPerPage={20} onPageChange={onPageChange} />
        </div>
    );
}
