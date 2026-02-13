import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './i18n';
import './App.css';

import { isTauri } from './utils/env';

// Show main window after frontend loads (avoids startup black screen)
if (isTauri()) {
    import('@tauri-apps/api/core').then(({ invoke }) => {
        invoke('show_main_window').catch(console.error);
    });
}

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
    <React.StrictMode>
        <App />
    </React.StrictMode>
);
