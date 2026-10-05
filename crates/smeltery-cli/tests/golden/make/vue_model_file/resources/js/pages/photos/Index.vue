<!-- `GET /photos` (`index` in `app/controllers/photos.rs`): every photo. -->
<script setup lang="ts">
import { Head, Link } from '@inertiajs/vue3';

import type { Photo } from '@/types/photo';

defineProps<{ photos: Photo[] }>();
</script>

<template>
    <main class="mx-auto max-w-3xl px-4 py-12 sm:px-6">
        <Head title="Photos" />
        <div class="flex flex-wrap items-center justify-between gap-4">
            <h1 class="text-3xl font-bold tracking-tight">Photos</h1>
            <Link href="/photos/create" class="btn-primary">New photo</Link>
        </div>
        <ul class="panel mt-8 divide-y divide-ash-200 p-0 dark:divide-forge-800">
            <li v-for="photo in photos" :key="photo.id">
                <Link :href="`/photos/${photo.id}`" class="block px-6 py-4 font-medium hover:bg-ash-100 focus-visible:outline-2 focus-visible:outline-molten-500 dark:hover:bg-forge-800">{{ photo.title }}</Link>
            </li>
            <li v-if="photos.length === 0" class="px-6 py-4 text-stone-600 dark:text-stone-400">No photos yet.</li>
        </ul>
    </main>
</template>
