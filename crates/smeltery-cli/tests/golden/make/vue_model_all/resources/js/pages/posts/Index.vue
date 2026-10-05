<!-- `GET /posts` (`index` in `app/controllers/posts.rs`): every post. -->
<script setup lang="ts">
import { Head, Link } from '@inertiajs/vue3';

import type { Post } from '@/types/post';

defineProps<{ posts: Post[] }>();
</script>

<template>
    <main class="mx-auto max-w-3xl px-4 py-12 sm:px-6">
        <Head title="Posts" />
        <div class="flex flex-wrap items-center justify-between gap-4">
            <h1 class="text-3xl font-bold tracking-tight">Posts</h1>
            <Link href="/posts/create" class="btn-primary">New post</Link>
        </div>
        <ul class="panel mt-8 divide-y divide-ash-200 p-0 dark:divide-forge-800">
            <li v-for="post in posts" :key="post.id">
                <Link :href="`/posts/${post.id}`" class="block px-6 py-4 font-medium hover:bg-ash-100 focus-visible:outline-2 focus-visible:outline-molten-500 dark:hover:bg-forge-800">{{ post.title }}</Link>
            </li>
            <li v-if="posts.length === 0" class="px-6 py-4 text-stone-600 dark:text-stone-400">No posts yet.</li>
        </ul>
    </main>
</template>
