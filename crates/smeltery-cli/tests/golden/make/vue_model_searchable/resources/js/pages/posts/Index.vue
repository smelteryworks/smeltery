<!-- `GET /posts?q=…&page=…` (`index` in `app/controllers/posts.rs`): the posts matching the search, ranked. -->
<script setup lang="ts">
import { Head, Link, router } from '@inertiajs/vue3';
import { ref, watch } from 'vue';

import type { Post } from '@/types/post';

/** One part of a highlighted text: rendered as text, the matched parts inside `<mark>` (never as HTML). */
interface Segment {
    text: string;
    matched: boolean;
}

/** A search result: the record, its relevance (`null` without search terms) and its highlighted columns. */
type Hit = Post & { _score: number | null; _highlights: Partial<Record<string, Segment[]>> };

/** One page of results (`smeltery::db::Page`). */
interface Results {
    items: Hit[];
    page: number;
    per_page: number;
    total: number;
    last_page: number;
}

const props = defineProps<{ posts: Results; q: string }>();
const search = ref(props.q);
let timer: ReturnType<typeof setTimeout> | undefined;

// Search as the visitor types, 300 ms after the last key; the page and its scroll position stay.
watch(search, (value) => {
    clearTimeout(timer);
    timer = setTimeout(() => {
        router.get('/posts', value ? { q: value } : {}, { preserveState: true, preserveScroll: true, replace: true });
    }, 300);
});

/** The highlighted parts of a result's title, or the whole text when the search has no terms. */
function parts(post: Hit): Segment[] {
    return post._highlights.title ?? [{ text: post.title, matched: false }];
}
</script>

<template>
    <main class="mx-auto max-w-3xl px-4 py-12 sm:px-6">
        <Head title="Posts" />
        <div class="flex flex-wrap items-center justify-between gap-4">
            <h1 class="text-3xl font-bold tracking-tight">Posts</h1>
            <Link href="/posts/create" class="btn-primary">New post</Link>
        </div>
        <input v-model="search" type="search" placeholder="Search posts" aria-label="Search posts" class="form-input mt-8 w-full" />
        <ul class="panel mt-4 divide-y divide-ash-200 p-0 dark:divide-forge-800">
            <li v-for="post in posts.items" :key="post.id">
                <Link :href="`/posts/${post.id}`" class="block px-6 py-4 font-medium hover:bg-ash-100 focus-visible:outline-2 focus-visible:outline-molten-500 dark:hover:bg-forge-800">
                    <template v-for="(part, i) in parts(post)" :key="i"><mark v-if="part.matched">{{ part.text }}</mark><template v-else>{{ part.text }}</template></template>
                </Link>
            </li>
            <li v-if="posts.items.length === 0" class="px-6 py-4 text-stone-600 dark:text-stone-400">
                {{ q ? 'No posts match your search.' : 'No posts yet.' }}
            </li>
        </ul>
        <nav v-if="posts.last_page > 1" class="mt-6 flex items-center justify-between gap-4 text-sm" aria-label="Pages">
            <Link v-if="posts.page > 1" href="/posts" :data="{ q, page: posts.page - 1 }" preserve-state class="btn-secondary">Previous</Link>
            <span v-else />
            <span class="text-stone-600 dark:text-stone-400">Page {{ posts.page }} of {{ posts.last_page }}</span>
            <Link v-if="posts.page < posts.last_page" href="/posts" :data="{ q, page: posts.page + 1 }" preserve-state class="btn-secondary">Next</Link>
            <span v-else />
        </nav>
    </main>
</template>
