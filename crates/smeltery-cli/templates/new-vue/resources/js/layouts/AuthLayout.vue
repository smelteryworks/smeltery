<!-- The narrow, centred column of the authentication pages: a title, a line under it and the form. -->
<script setup lang="ts">
import { Head } from '@inertiajs/vue3';

defineProps<{ title: string; description: string }>();
</script>

<template>
    <main class="mx-auto max-w-md px-4 py-12 sm:py-16">
        <Head :title="title" />
        <h1 class="text-center text-2xl font-bold tracking-tight">{{ title }}</h1>
        <p class="mt-2 text-center text-sm text-stone-600 dark:text-stone-400">{{ description }}</p>
        <slot />
    </main>
</template>
