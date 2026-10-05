<!-- The first page after logging in. `activity` is a deferred prop: the client loads it right after the page. -->
<script setup lang="ts">
import { Deferred, Head, Link, usePage } from '@inertiajs/vue3';
import { computed } from 'vue';

import AppCard from '@/components/AppCard.vue';

/** `Activity` in `app/controllers/dashboard.rs`. */
interface Activity {
    accounts: number;
    member_since: string | null;
}

defineProps<{ activity?: Activity }>();

const page = usePage();
const name = computed(() => page.props.auth.user?.name ?? '');
</script>

<template>
    <main class="mx-auto max-w-6xl px-4 py-12 sm:px-6">
        <Head title="Dashboard" />
        <p class="mono-label">Dashboard</p>
        <h1 class="mt-2 text-3xl font-bold tracking-tight">
            Welcome, <span class="text-molten-700 dark:text-molten-500">{{ name }}</span>
        </h1>
        <p class="mt-4 text-stone-700 dark:text-stone-300">You are logged in as {{ name }}.</p>
        <div class="mt-8 grid gap-4 md:grid-cols-3">
            <AppCard title="This page">
                <p>
                    <code>resources/js/pages/Dashboard.vue</code>, behind the <code>auth</code> and <code>verified</code> middleware in
                    <code>routes/web.rs</code>.
                </p>
            </AppCard>
            <AppCard title="Your account">
                <p>
                    Sign-up, log-in, password resets, email verification and two-factor authentication come from Temper:
                    <code>app/providers/temper.rs</code> turns them on, <code>app/actions/temper/</code> holds the forms. Change your details
                    under <Link href="/settings/profile" class="link">Settings</Link>.
                </p>
            </AppCard>
            <AppCard title="Recent activity">
                <Deferred data="activity">
                    <template #fallback>
                        <p class="animate-pulse text-stone-500 dark:text-stone-400">Loading the activity…</p>
                    </template>
                    <dl v-if="activity" class="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
                        <dt class="text-stone-600 dark:text-stone-400">Accounts</dt>
                        <dd class="font-mono">{{ activity.accounts }}</dd>
                        <dt class="text-stone-600 dark:text-stone-400">Member since</dt>
                        <dd class="font-mono">{{ activity.member_since ?? '-' }}</dd>
                    </dl>
                </Deferred>
                <p class="mt-2 text-xs text-stone-600 dark:text-stone-400">A deferred prop, loaded after the page appeared.</p>
            </AppCard>
        </div>
    </main>
</template>
