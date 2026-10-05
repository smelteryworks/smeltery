import '../css/app.css';

import { createInertiaApp } from '@inertiajs/vue3';

import AppLayout from '@/layouts/AppLayout.vue';

const appName = import.meta.env.VITE_APP_NAME || 'Smeltery';

// The pages live in resources/js/pages/: a controller's `alloy::render("auth/Login")` shows pages/auth/Login.vue
// (@inertiajs/vite adds the resolver). Every page sits in the app layout.
createInertiaApp({
    title: (title) => (title ? `${title} · ${appName}` : appName),
    layout: () => AppLayout,
    progress: { color: '#ff4a1c' },
});
