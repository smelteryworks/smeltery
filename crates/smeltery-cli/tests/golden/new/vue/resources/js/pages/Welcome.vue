<!-- The welcome page. Replace it with your own: the route is in routes/web.rs, the controller in app/controllers/home.rs. -->
<script setup lang="ts">
import { router, usePage } from '@inertiajs/vue3';
import { ref } from 'vue';

import AppCard from '@/components/AppCard.vue';
import ForgeIngot from '@/components/ForgeIngot.vue';

/** `ForgeReading` in `app/controllers/home.rs`. */
interface ForgeReading {
    temperature: number;
    reading: number;
    served_at: string;
}

defineProps<{ version: string; forge?: ForgeReading }>();

const page = usePage();
// Browser state: it survives the partial reload below, because only the `forge` prop changes.
const asked = ref(0);
const asking = ref(false);

function askTheForge() {
    asked.value += 1;
    router.reload({
        only: ['forge'],
        onStart: () => (asking.value = true),
        onFinish: () => (asking.value = false),
    });
}
</script>

<template>
    <main class="mx-auto max-w-6xl px-4 sm:px-6">
        <section class="grid items-center gap-10 py-14 sm:py-20 lg:grid-cols-[1.15fr_1fr]">
            <div>
                <p class="inline-flex items-center gap-2 rounded-full border border-ash-200 bg-white/70 px-3 py-1 font-mono text-xs text-stone-700 dark:border-forge-700 dark:bg-forge-900/70 dark:text-stone-300">
                    <span class="h-1.5 w-1.5 rounded-full bg-molten-500 shadow-[0_0_8px_2px_rgb(255_74_28/0.7)]" aria-hidden="true"></span>
                    Rust · Smeltery {{ version }} · Vue + Inertia
                </p>
                <h1 class="mt-6 text-4xl font-bold tracking-tight text-balance sm:text-5xl lg:text-6xl">
                    {{ page.props.app.name }} is <span class="text-molten-700 dark:text-molten-500">running</span> on
                    <span class="bg-linear-to-r from-molten-800 to-molten-600 bg-clip-text text-transparent dark:from-molten-500 dark:to-gold-400">Smeltery</span>
                </h1>
                <p class="mt-6 max-w-xl text-lg text-stone-700 dark:text-stone-300">
                    The forge is hot. Rust answers on the server, Vue draws the pages, and Inertia carries the props between them.
                </p>
                <div class="mt-8 flex flex-wrap gap-3">
                    <a href="https://github.com/smelteryworks/smeltery#readme" class="btn-primary">Read the guide</a>
                    <a href="https://docs.rs/smeltery" class="btn-secondary">API docs on docs.rs</a>
                </div>
            </div>
            <ForgeIngot />
        </section>

        <section aria-labelledby="next-steps" class="pb-16">
            <h2 id="next-steps" class="mono-label">Next steps</h2>
            <div class="mt-4 grid gap-4 md:grid-cols-2 lg:grid-cols-3">
                <AppCard title="Edit this page">
                    <p>
                        This page is <code>resources/js/pages/Welcome.vue</code>; its route lives in <code>routes/web.rs</code> and its props
                        come from <code>app/controllers/home.rs</code>. The Vite dev server updates the page as you save.
                    </p>
                </AppCard>
                <AppCard title="Make a model">
                    <p>
                        Run <code>smeltery make:model Post title:string --all</code> for a model, migration, factory, seeder, controller, pages and routes, then
                        <code>smeltery migrate</code>.
                    </p>
                </AppCard>
                <AppCard title="Watch the agents">
                    <p>Agents, jobs and the schedule run in this process. <a href="/_watchfire" class="link">Open the Watchfire dashboard</a>.</p>
                </AppCard>
            </div>
        </section>

        <section aria-labelledby="forge" class="pb-20">
            <h2 id="forge" class="mono-label">A partial reload</h2>
            <div class="panel mt-4 flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
                <div class="text-sm leading-relaxed text-stone-700 dark:text-stone-300">
                    <h3 class="mb-2 text-base font-semibold text-forge-900 dark:text-ash-100">Ask the forge</h3>
                    <p>
                        The reading is an optional prop: the server computes it only when a partial reload asks for
                        <code class="rounded bg-ash-100 px-1 py-0.5 font-mono text-xs text-forge-900 dark:bg-forge-800 dark:text-gold-300">forge</code>.
                    </p>
                    <p class="mt-2" aria-live="polite">
                        <template v-if="forge">
                            Reading <span class="font-mono">#{{ forge.reading }}</span>:
                            <span class="font-mono text-2xl font-bold text-molten-700 dark:text-molten-500">{{ forge.temperature }} °C</span> at
                            <span class="font-mono">{{ forge.served_at }}</span> UTC.
                        </template>
                        <template v-else>No reading yet.</template>
                    </p>
                    <p class="mt-2 text-xs text-stone-600 dark:text-stone-400">
                        Asked {{ asked }} {{ asked === 1 ? 'time' : 'times' }} from this page; the count lives in the browser and survives each reload.
                    </p>
                </div>
                <button type="button" class="btn-primary shrink-0" :disabled="asking" @click="askTheForge">
                    {{ asking ? 'Reading…' : 'Ask the forge' }}
                </button>
            </div>
        </section>
    </main>
</template>
