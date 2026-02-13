/**
 * Robust clipboard copy utility.
 *
 * Falls back to execCommand('copy') in non-secure contexts (HTTP / Docker IP).
 */
export async function copyToClipboard(text: string): Promise<boolean> {
    // 1. Modern Clipboard API
    if (navigator.clipboard && window.isSecureContext) {
        try {
            await navigator.clipboard.writeText(text);
            return true;
        } catch (err) {
            console.error('Clipboard API copy failed:', err);
        }
    }

    // 2. Fallback: execCommand('copy')
    try {
        const textArea = document.createElement('textarea');
        textArea.value = text;
        textArea.style.position = 'fixed';
        textArea.style.left = '-9999px';
        textArea.style.top = '0';
        document.body.appendChild(textArea);
        textArea.focus();
        textArea.select();
        const successful = document.execCommand('copy');
        document.body.removeChild(textArea);
        return successful;
    } catch (err) {
        console.error('execCommand copy failed:', err);
        return false;
    }
}
