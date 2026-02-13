import { isTauri } from './env';

/**
 * Enter mini view mode
 */
export const enterMiniMode = async (contentHeight: number, shouldCenter: boolean = false) => {
    if (!isTauri()) return;
    try {
        const { getCurrentWindow, LogicalSize } = await import('@tauri-apps/api/window');
        const win = getCurrentWindow();

        await win.setDecorations(false);
        await win.setSize(new LogicalSize(300, contentHeight + 2));
        await win.setAlwaysOnTop(true);
        await win.setShadow(true);
        await win.setResizable(false);

        if (shouldCenter) {
            await win.center();
        }
    } catch (error) {
        console.error('Failed to enter mini mode:', error);
    }
};

/**
 * Exit mini view mode and restore default window state
 */
export const exitMiniMode = async () => {
    if (!isTauri()) return;
    try {
        const { getCurrentWindow, LogicalSize } = await import('@tauri-apps/api/window');
        const win = getCurrentWindow();

        await win.setSize(new LogicalSize(1200, 800));
        await win.setAlwaysOnTop(false);
        await win.center();
        await win.setDecorations(true);
        await win.setResizable(true);
    } catch (error) {
        console.error('Failed to exit mini mode:', error);
    }
};

/**
 * Ensure window is in valid full view state (self-healing on startup)
 */
export const ensureFullViewState = async () => {
    if (!isTauri()) return;
    try {
        const { getCurrentWindow, LogicalSize } = await import('@tauri-apps/api/window');
        const win = getCurrentWindow();
        const size = await win.outerSize();

        if (size.width < 500) {
            await win.setSize(new LogicalSize(1200, 800));
            await win.center();
        }
        await win.setDecorations(true);
        await win.setResizable(true);
        await win.setAlwaysOnTop(false);
    } catch (error) {
        console.error('Failed to ensure full view state:', error);
    }
};
