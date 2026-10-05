import { http } from '@inertiajs/core';
import { configureEcho, echo, echoIsConfigured } from '@laravel/echo-vue';

/** The `XSRF-TOKEN` cookie's value: the app's CSRF token, read at every authorization (it changes when you log in). */
function xsrfToken(): string {
    const match = document.cookie.match(/(?:^|; )XSRF-TOKEN=([^;]*)/);
    return match ? decodeURIComponent(match[1]) : '';
}

/**
 * Connects Echo to the app's WebSocket server (Anvil, `.anvil(…)` in `bootstrap/app.rs`). Anvil runs inside the
 * app, so the socket goes to the page's own host and port; `key` is the app's public Anvil key, shared with every
 * page as `app.anvil_key` (`app/providers/alloy.rs`). Private and presence channels are authorized at
 * `/broadcasting/auth` with the session cookie and the CSRF token. `app.ts` calls it once, before the first page.
 */
export function startEcho(key: string): void {
    if (echoIsConfigured() || !key) {
        return;
    }
    const secure = window.location.protocol === 'https:';
    const port = Number(window.location.port) || (secure ? 443 : 80);
    configureEcho({
        broadcaster: 'pusher',
        key,
        cluster: 'mt1', // pusher-js needs a cluster name; wsHost decides where it connects
        wsHost: window.location.hostname,
        wsPort: port,
        wssPort: port,
        forceTLS: secure,
        enabledTransports: ['ws', 'wss'],
        disableStats: true,
        withoutInterceptors: true,
        channelAuthorization: {
            customHandler: (
                { socketId, channelName }: { socketId: string; channelName: string },
                callback: (error: Error | null, data: { auth: string; channel_data?: string } | null) => void,
            ) => {
                fetch('/broadcasting/auth', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json', Accept: 'application/json', 'X-XSRF-TOKEN': xsrfToken() },
                    body: JSON.stringify({ socket_id: socketId, channel_name: channelName }),
                })
                    .then((response) => (response.ok ? response.json() : Promise.reject(new Error(`/broadcasting/auth: ${response.status}`))))
                    .then((data) => callback(null, data))
                    .catch((error: Error) => callback(error, null));
            },
        },
    });
    // Inertia's requests carry this tab's socket id, so the server can leave it out of an event it sends while
    // handling the request (`anvil.send(&event).except(socket)` with a `SocketId` argument).
    http.onRequest((config) => {
        const id = echo().socketId();
        if (id) {
            config.headers = { ...config.headers, 'X-Socket-ID': id };
        }
        return config;
    });
}
