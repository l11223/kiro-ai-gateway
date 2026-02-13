import { Brain } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import HelpTooltip from '../common/HelpTooltip';

interface ThinkingBudgetConfig {
    mode: string;
    custom_value?: number;
}

interface ThinkingBudgetProps {
    config: ThinkingBudgetConfig;
    onChange: (config: ThinkingBudgetConfig) => void;
}

export default function ThinkingBudget({ config, onChange }: ThinkingBudgetProps) {
    const { t } = useTranslation();
    const modes = ['auto', 'passthrough', 'custom', 'adaptive'];

    return (
        <div className="bg-white dark:bg-base-100 rounded-xl p-4 shadow-sm border border-gray-100 dark:border-base-200">
            <h3 className="text-sm font-semibold text-gray-900 dark:text-base-content mb-3 flex items-center gap-2">
                <Brain className="w-4 h-4 text-purple-500" />
                {t('settings.thinking_budget', 'Thinking Budget')}
                <HelpTooltip text={t('settings.thinking_budget_help', 'Controls how much thinking budget is allocated for AI responses')} />
            </h3>
            <div className="space-y-3">
                <div className="grid grid-cols-2 gap-2">
                    {modes.map(mode => (
                        <button key={mode} onClick={() => onChange({ ...config, mode })}
                            className={`px-3 py-2 rounded-lg text-xs font-medium transition-all ${config.mode === mode ? 'bg-purple-100 dark:bg-purple-900/20 text-purple-700 dark:text-purple-300 border border-purple-200 dark:border-purple-800' : 'bg-gray-50 dark:bg-base-200 text-gray-600 dark:text-gray-400 border border-transparent hover:border-gray-200 dark:hover:border-base-300'}`}>
                            {mode.charAt(0).toUpperCase() + mode.slice(1)}
                        </button>
                    ))}
                </div>
                {config.mode === 'custom' && (
                    <div>
                        <label className="text-xs text-gray-500 dark:text-gray-400 mb-1 block">{t('settings.custom_value', 'Custom Value')}</label>
                        <input type="number" value={config.custom_value || 0} onChange={(e) => onChange({ ...config, custom_value: parseInt(e.target.value) || 0 })}
                            className="w-full px-3 py-2 text-sm border border-gray-200 dark:border-base-300 rounded-lg bg-white dark:bg-base-200 text-gray-900 dark:text-base-content" />
                    </div>
                )}
            </div>
        </div>
    );
}
