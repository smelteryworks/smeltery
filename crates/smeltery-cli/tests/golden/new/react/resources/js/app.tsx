// React Fast Refresh in the dev server (no tag needed in the root template).
import '@vitejs/plugin-react/preamble';
import '../css/app.css';

import { createInertiaApp } from '@inertiajs/react';

import AppLayout from '@/layouts/app-layout';

const appName = import.meta.env.VITE_APP_NAME || 'Smeltery';

// The pages live in resources/js/pages/: a controller's `alloy::render("auth/login")` shows pages/auth/login.tsx
// (@inertiajs/vite adds the resolver). Every page sits in the app layout.
createInertiaApp({
    title: (title) => (title ? `${title} · ${appName}` : appName),
    layout: () => AppLayout,
    progress: { color: '#ff4a1c' },
});
