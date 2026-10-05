<!-- `GET /posts/{id}/edit` (`edit` in `app/controllers/posts.rs`): the form that `update` saves. -->
<script setup lang="ts">
import { Head, Link, useForm } from '@inertiajs/vue3';

import type { Post } from '@/types/post';

const props = defineProps<{ post: Post }>();

const form = useForm({
    title: props.post.title,
    body: props.post.body ?? '',
    views: String(props.post.views),
    rating: props.post.rating === null ? '' : String(props.post.rating),
    published: props.post.published,
});

function submit() {
    // Number fields hold the text of their input; the server reads numbers (an empty field is a missing value).
    form.transform((data) => ({
        ...data,
        views: data.views === '' ? '' : Number(data.views),
        rating: data.rating === '' ? '' : Number(data.rating),
    }));
    form.put(`/posts/${props.post.id}`);
}
</script>

<template>
    <main class="mx-auto max-w-3xl px-4 py-12 sm:px-6">
        <Head title="Edit post" />
        <h1 class="text-3xl font-bold tracking-tight">Edit post {{ post.id }}</h1>
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <div>
                <label for="title" class="form-label">Title</label>
                <input
                    id="title"
                    v-model="form.title"
                    name="title"
                    type="text"
                    required
                    class="form-input"
                    :aria-invalid="form.errors.title ? true : undefined"
                    :aria-describedby="form.errors.title ? 'title-error' : undefined"
                />
                <p v-if="form.errors.title" id="title-error" class="form-error">{{ form.errors.title }}</p>
            </div>
            <div>
                <label for="body" class="form-label">Body</label>
                <textarea
                    id="body"
                    v-model="form.body"
                    name="body"
                    class="form-input"
                    :aria-invalid="form.errors.body ? true : undefined"
                    :aria-describedby="form.errors.body ? 'body-error' : undefined"
                />
                <p v-if="form.errors.body" id="body-error" class="form-error">{{ form.errors.body }}</p>
            </div>
            <div>
                <label for="views" class="form-label">Views</label>
                <input
                    id="views"
                    v-model="form.views"
                    name="views"
                    type="number"
                    required
                    class="form-input"
                    :aria-invalid="form.errors.views ? true : undefined"
                    :aria-describedby="form.errors.views ? 'views-error' : undefined"
                />
                <p v-if="form.errors.views" id="views-error" class="form-error">{{ form.errors.views }}</p>
            </div>
            <div>
                <label for="rating" class="form-label">Rating</label>
                <input
                    id="rating"
                    v-model="form.rating"
                    name="rating"
                    type="number"
                    step="any"
                    class="form-input"
                    :aria-invalid="form.errors.rating ? true : undefined"
                    :aria-describedby="form.errors.rating ? 'rating-error' : undefined"
                />
                <p v-if="form.errors.rating" id="rating-error" class="form-error">{{ form.errors.rating }}</p>
            </div>
            <div>
                <label for="published" class="form-label">Published</label>
                <input
                    id="published"
                    v-model="form.published"
                    name="published"
                    type="checkbox"
                    class="mt-2 h-4 w-4 accent-molten-700"
                    :aria-invalid="form.errors.published ? true : undefined"
                    :aria-describedby="form.errors.published ? 'published-error' : undefined"
                />
                <p v-if="form.errors.published" id="published-error" class="form-error">{{ form.errors.published }}</p>
            </div>
            <div class="flex items-center gap-3">
                <button type="submit" class="btn-primary" :disabled="form.processing">Save</button>
                <Link href="/posts" class="btn-secondary">Cancel</Link>
            </div>
        </form>
    </main>
</template>
