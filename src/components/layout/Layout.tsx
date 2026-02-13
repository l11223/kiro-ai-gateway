import { Outlet } from 'react-router-dom';
import Navbar from '../navbar/Navbar';
import BackgroundTaskRunner from '../common/BackgroundTaskRunner';
import ToastContainer from '../common/ToastContainer';
import DebugConsole from '../debug/DebugConsole';
import { useViewStore } from '../../stores/useViewStore';
import MiniView from './MiniView';
import { useEffect } from 'react';
import { isTauri } from '../../utils/env';
import { ensureFullViewState } from '../../utils/windowManager';

export default function Layout() {
    const { isMiniView } = useViewStore();

    useEffect(() => {
        if (!isMiniView && isTauri()) {
            ensureFullViewState();
        }
    }, [isMiniView]);

    if (isMiniView) {
        return (
            <>
                <BackgroundTaskRunner />
                <ToastContainer />
                <MiniView />
            </>
        );
    }

    return (
        <div className="h-screen flex flex-col bg-[#FAFBFC] dark:bg-base-300">
            {isTauri() && (
                <div
                    className="fixed top-0 left-0 right-0 h-9"
                    style={{ zIndex: 9999, backgroundColor: 'rgba(0,0,0,0.001)', cursor: 'default', userSelect: 'none' }}
                    data-tauri-drag-region
                    onMouseDown={() => {
                        import('@tauri-apps/api/window').then(({ getCurrentWindow }) => getCurrentWindow().startDragging());
                    }}
                />
            )}
            <BackgroundTaskRunner />
            <ToastContainer />
            <DebugConsole />
            <Navbar />
            <main className="flex-1 overflow-hidden flex flex-col relative">
                <Outlet />
            </main>
        </div>
    );
}
