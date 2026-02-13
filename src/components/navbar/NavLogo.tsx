import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Zap } from 'lucide-react';

export function NavLogo() {
    const { t } = useTranslation();
    return (
        <Link to="/" draggable="false" className="flex w-full min-w-0 items-center gap-2 text-xl font-semibold text-gray-900 dark:text-base-content">
            <Zap className="w-7 h-7 text-blue-500 shrink-0" />
            <span className="hidden @[200px]/logo:inline text-nowrap">{t('common.app_name', 'Kiro AI Gateway')}</span>
        </Link>
    );
}
