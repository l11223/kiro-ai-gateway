import { ChevronLeft, ChevronRight } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface PaginationProps {
    currentPage: number;
    totalPages: number;
    onPageChange: (page: number) => void;
    totalItems: number;
    itemsPerPage: number;
    onPageSizeChange?: (pageSize: number) => void;
    pageSizeOptions?: number[];
}

export default function Pagination({
    currentPage, totalPages, onPageChange, totalItems, itemsPerPage,
    onPageSizeChange, pageSizeOptions = [10, 20, 50, 100]
}: PaginationProps) {
    const { t } = useTranslation();

    if (totalPages <= 1 && !onPageSizeChange) return null;

    let startPage = Math.max(1, currentPage - 2);
    const endPage = Math.min(totalPages, startPage + 4);
    if (endPage - startPage < 4) startPage = Math.max(1, endPage - 4);

    const pages: number[] = [];
    for (let i = startPage; i <= endPage; i++) pages.push(i);

    const startIndex = (currentPage - 1) * itemsPerPage + 1;
    const endIndex = Math.min(currentPage * itemsPerPage, totalItems);

    return (
        <div className="flex items-center justify-between px-6 py-3">
            <div className="flex flex-1 justify-between sm:hidden">
                <button onClick={() => onPageChange(currentPage - 1)} disabled={currentPage === 1}
                    className={`relative inline-flex items-center rounded-md border border-gray-300 dark:border-gray-600 px-4 py-2 text-sm font-medium ${currentPage === 1 ? 'bg-gray-100 dark:bg-gray-800 text-gray-400 cursor-not-allowed' : 'bg-white dark:bg-base-100 text-gray-700 dark:text-gray-200 hover:bg-gray-50 dark:hover:bg-base-200'}`}>
                    {t('common.prev_page')}
                </button>
                <button onClick={() => onPageChange(currentPage + 1)} disabled={currentPage === totalPages}
                    className={`relative ml-3 inline-flex items-center rounded-md border border-gray-300 dark:border-gray-600 px-4 py-2 text-sm font-medium ${currentPage === totalPages ? 'bg-gray-100 dark:bg-gray-800 text-gray-400 cursor-not-allowed' : 'bg-white dark:bg-base-100 text-gray-700 dark:text-gray-200 hover:bg-gray-50 dark:hover:bg-base-200'}`}>
                    {t('common.next_page')}
                </button>
            </div>
            <div className="hidden sm:flex sm:flex-1 sm:items-center sm:justify-between">
                <div className="flex items-center gap-4">
                    <p className="text-sm text-gray-700 dark:text-gray-400">
                        {startIndex}-{endIndex} / {totalItems}
                    </p>
                    {onPageSizeChange && (
                        <select value={itemsPerPage} onChange={(e) => onPageSizeChange(parseInt(e.target.value))}
                            className="px-2 py-1 text-sm border border-gray-300 dark:border-gray-600 rounded-md bg-white dark:bg-base-100 text-gray-900 dark:text-base-content">
                            {pageSizeOptions.map(size => <option key={size} value={size}>{size}</option>)}
                        </select>
                    )}
                </div>
                <nav className="isolate inline-flex -space-x-px rounded-md shadow-sm">
                    <button onClick={() => onPageChange(currentPage - 1)} disabled={currentPage === 1}
                        className={`relative inline-flex items-center rounded-l-md px-2 py-2 text-gray-400 ring-1 ring-inset ring-gray-300 dark:ring-gray-600 hover:bg-gray-50 dark:hover:bg-base-200 ${currentPage === 1 ? 'cursor-not-allowed opacity-50' : ''}`}>
                        <ChevronLeft className="h-4 w-4" />
                    </button>
                    {startPage > 1 && (
                        <>
                            <button onClick={() => onPageChange(1)} className="relative inline-flex items-center px-4 py-2 text-sm font-semibold text-gray-900 dark:text-gray-200 ring-1 ring-inset ring-gray-300 dark:ring-gray-600 hover:bg-gray-50 dark:hover:bg-base-200">1</button>
                            {startPage > 2 && <span className="relative inline-flex items-center px-4 py-2 text-sm font-semibold text-gray-700 dark:text-gray-400 ring-1 ring-inset ring-gray-300 dark:ring-gray-600">...</span>}
                        </>
                    )}
                    {pages.map(page => (
                        <button key={page} onClick={() => onPageChange(page)}
                            className={`relative inline-flex items-center px-4 py-2 text-sm font-semibold ${page === currentPage ? 'z-10 bg-blue-600 text-white' : 'text-gray-900 dark:text-gray-200 ring-1 ring-inset ring-gray-300 dark:ring-gray-600 hover:bg-gray-50 dark:hover:bg-base-200'}`}>
                            {page}
                        </button>
                    ))}
                    {endPage < totalPages && (
                        <>
                            {endPage < totalPages - 1 && <span className="relative inline-flex items-center px-4 py-2 text-sm font-semibold text-gray-700 dark:text-gray-400 ring-1 ring-inset ring-gray-300 dark:ring-gray-600">...</span>}
                            <button onClick={() => onPageChange(totalPages)} className="relative inline-flex items-center px-4 py-2 text-sm font-semibold text-gray-900 dark:text-gray-200 ring-1 ring-inset ring-gray-300 dark:ring-gray-600 hover:bg-gray-50 dark:hover:bg-base-200">{totalPages}</button>
                        </>
                    )}
                    <button onClick={() => onPageChange(currentPage + 1)} disabled={currentPage === totalPages}
                        className={`relative inline-flex items-center rounded-r-md px-2 py-2 text-gray-400 ring-1 ring-inset ring-gray-300 dark:ring-gray-600 hover:bg-gray-50 dark:hover:bg-base-200 ${currentPage === totalPages ? 'cursor-not-allowed opacity-50' : ''}`}>
                        <ChevronRight className="h-4 w-4" />
                    </button>
                </nav>
            </div>
        </div>
    );
}
