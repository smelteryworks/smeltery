<!-- `GET /posts/{id}` (`show` in `app/controllers/posts.rs`): one post. -->
<script setup lang="ts">
import { Head, Link } from '@inertiajs/vue3';

import type { Post } from '@/types/post';

defineProps<{ post: Post }>();
</script>

<template>
    <main class="mx-auto max-w-3xl px-4 py-12 sm:px-6">
        <Head title="Post" />
        <h1 class="text-3xl font-bold tracking-tight">Post {{ post.id }}</h1>
        <dl class="panel mt-8 grid gap-x-6 gap-y-3 sm:grid-cols-[10rem_1fr]">
            <dt class="mono-label pt-0.5">Title</dt>
            <dd>{{ post.title }}</dd>
            <dt class="mono-label pt-0.5">Body</dt>
            <dd>{{ post.body }}</dd>
            <dt class="mono-label pt-0.5">Views</dt>
            <dd>{{ post.views }}</dd>
            <dt class="mono-label pt-0.5">Rating</dt>
            <dd>{{ post.rating }}</dd>
            <dt class="mono-label pt-0.5">Published</dt>
            <dd>{{ post.published ? 'Yes' : 'No' }}</dd>
        </dl>
        <div class="mt-6 flex flex-wrap items-center gap-3">
            <Link :href="`/posts/${post.id}/edit`" class="btn-primary">Edit</Link>
            <Link href="/posts" class="btn-secondary">All posts</Link>
            <Link :href="`/posts/${post.id}`" method="delete" as="button" class="btn-secondary ml-auto text-red-700 dark:text-red-300">Delete</Link>
        </div>
    </main>
</template>
