<!-- The settings pages: a title, the tabs and the page's panel. -->
<script setup lang="ts">
import { Head, Link } from '@inertiajs/vue3';

defineProps<{ title: string; current: string }>();

const tabs = [
    { href: '/settings/profile', label: 'Profile' },
    { href: '/settings/password', label: 'Password' },
    { href: '/settings/two-factor', label: 'Two-factor authentication' },
];
</script>

<template>
    <main class="mx-auto max-w-xl px-4 py-12 sm:px-6">
        <Head :title="title" />
        <p class="mono-label">Settings</p>
        <h1 class="mt-2 text-3xl font-bold tracking-tight">{{ title }}</h1>
        <nav class="mt-6 flex flex-wrap gap-2 text-sm" aria-label="Settings">
            <Link
                v-for="tab in tabs"
                :key="tab.href"
                :href="tab.href"
                class="rounded-lg px-3 py-2 font-medium hover:bg-ash-100 focus-visible:outline-2 focus-visible:outline-molten-500 dark:hover:bg-forge-900"
                :aria-current="tab.href === current ? 'page' : undefined"
            >
                {{ tab.label }}
            </Link>
        </nav>
        <slot />
    </main>
</template>
