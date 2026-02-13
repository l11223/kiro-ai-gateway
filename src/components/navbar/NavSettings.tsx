import { Sun, Moon, LogOut, Minimize2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { LanguageDropdown, MoreDropdown } from './NavDropdowns';
import { LANGUAGES } from './constants';
import { isTauri } from '../../utils/env';
import { useViewStore } from '../../stores/useViewStore';

interface NavSettingsProps {
    theme: 'light' | 'dark';
    currentLanguage: string;
    onThemeToggle: (event: React.MouseEvent<HTMLButtonElement>) => void;
    onLanguageChange: (langCode: string) => void;
}

export function NavSettings({ theme, currentLanguage, onThemeToggle, onLanguageChange }: NavSettingsProps) {
    const { t } = useTranslation();
    const { setMiniView } = useViewStore();

    return (
        <>
            {/* Buttons (≥ 480px) */}
            <div className="hidden min-[480px]:flex items-center gap-2">
                <button onClick={() => setMiniView(true)}
                    className="w-10 h-10 rounded-full bg-gray-100 dark:bg-base-200 hover:bg-gray-200 dark:hover:bg-base-100 flex items-center justify-center transition-colors"
                    title={t('nav.mini_view', 'Mini View')}>
                    <Minimize2 className="w-5 h-5 text-gray-700 dark:text-gray-300" />
                </button>
                <button onClick={onThemeToggle}
                    className="w-10 h-10 rounded-full bg-gray-100 dark:bg-base-200 hover:bg-gray-200 dark:hover:bg-base-100 flex items-center justify-center transition-colors"
                    title={theme === 'light' ? t('nav.theme_to_dark', 'Dark Mode') : t('nav.theme_to_light', 'Light Mode')}>
                    {theme === 'light' ? <Moon className="w-5 h-5 text-gray-700 dark:text-gray-300" /> : <Sun className="w-5 h-5 text-gray-700 dark:text-gray-300" />}
                </button>
                <LanguageDropdown currentLanguage={currentLanguage} languages={LANGUAGES} onLanguageChange={onLanguageChange} />
                {!isTauri() && (
                    <button onClick={() => { sessionStorage.removeItem('kiro_admin_api_key'); window.location.reload(); }}
                        className="w-10 h-10 rounded-full bg-red-50 dark:bg-red-900/20 hover:bg-red-100 dark:hover:bg-red-900/40 flex items-center justify-center transition-colors"
                        title={t('nav.logout', 'Logout')}>
                        <LogOut className="w-5 h-5 text-red-600 dark:text-red-400" />
                    </button>
                )}
            </div>

            {/* More menu (< 480px) */}
            <div className="min-[480px]:hidden">
                <MoreDropdown theme={theme} currentLanguage={currentLanguage} languages={LANGUAGES}
                    onThemeToggle={onThemeToggle} onLanguageChange={onLanguageChange} />
            </div>
        </>
    );
}
