import inertia from '@inertiajs/vite';
import tailwindcss from '@tailwindcss/vite';
import vue from '@vitejs/plugin-vue';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import { defineConfig, type Plugin } from 'vite';

/**
 * Smeltery's side of Vite: builds into public/build with a manifest (the app's `@vite` reads it), and while the dev
 * server runs, writes its URL to storage/framework/vite.hot (read by debug builds only).
 */
function smeltery(input: string[]): Plugin {
    const hot = 'storage/framework/vite.hot';
    return {
        name: 'smeltery',
        config: (_, { command }) => ({
            base: command === 'build' ? '/build/' : '/',
            // public/ is the app's static folder, and the build goes inside it.
            publicDir: false,
            build: {
                manifest: 'manifest.json',
                outDir: 'public/build',
                emptyOutDir: true,
                rolldownOptions: { input },
            },
            server: { host: '127.0.0.1', port: 5173, strictPort: true, origin: 'http://127.0.0.1:5173' },
        }),
        configureServer(server) {
            server.httpServer?.once('listening', () => {
                fs.mkdirSync('storage/framework', { recursive: true });
                fs.writeFileSync(hot, 'http://127.0.0.1:5173');
            });
            const clean = () => fs.rmSync(hot, { force: true });
            process.on('exit', clean);
            for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP'] as const) {
                process.on(signal, () => process.exit());
            }
        },
    };
}

export default defineConfig({
    // `ssr: false`: pages render in the browser; the app runs no Node.js server.
    plugins: [smeltery(['resources/js/app.ts']), inertia({ ssr: false }), vue(), tailwindcss()],
    resolve: {
        alias: { '@': fileURLToPath(new URL('./resources/js', import.meta.url)) },
    },
});
