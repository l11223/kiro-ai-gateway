/**
 * Detect if the app is running in a Tauri environment
 */
export const isTauri = (): boolean => {
    return typeof window !== 'undefined' &&
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (!!(window as any).__TAURI_INTERNALS__ || !!(window as any).__TAURI__);
};

/**
 * Detect if running on Linux
 */
export const isLinux = (): boolean => {
    return navigator.userAgent.toLowerCase().includes('linux');
};
