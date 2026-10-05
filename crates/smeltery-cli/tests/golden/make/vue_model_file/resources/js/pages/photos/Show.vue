<!-- `GET /photos/{id}` (`show` in `app/controllers/photos.rs`): one photo. -->
<script setup lang="ts">
import { Head, Link } from '@inertiajs/vue3';

import type { Photo } from '@/types/photo';

defineProps<{ photo: Photo }>();
</script>

<template>
    <main class="mx-auto max-w-3xl px-4 py-12 sm:px-6">
        <Head title="Photo" />
        <h1 class="text-3xl font-bold tracking-tight">Photo {{ photo.id }}</h1>
        <dl class="panel mt-8 grid gap-x-6 gap-y-3 sm:grid-cols-[10rem_1fr]">
            <dt class="mono-label pt-0.5">Title</dt>
            <dd>{{ photo.title }}</dd>
            <dt class="mono-label pt-0.5">Image</dt>
            <dd>
                <a v-if="photo.image" :href="`/storage/${photo.image}`" class="link break-all">{{ photo.image }}</a>
            </dd>
            <dt class="mono-label pt-0.5">Scan</dt>
            <dd>
                <a v-if="photo.scan" :href="`/storage/${photo.scan}`" class="link break-all">{{ photo.scan }}</a>
            </dd>
            <dt class="mono-label pt-0.5">Public</dt>
            <dd>{{ photo.public ? 'Yes' : 'No' }}</dd>
        </dl>
        <div class="mt-6 flex flex-wrap items-center gap-3">
            <Link :href="`/photos/${photo.id}/edit`" class="btn-primary">Edit</Link>
            <Link href="/photos" class="btn-secondary">All photos</Link>
            <Link :href="`/photos/${photo.id}`" method="delete" as="button" class="btn-secondary ml-auto text-red-700 dark:text-red-300">Delete</Link>
        </div>
    </main>
</template>
