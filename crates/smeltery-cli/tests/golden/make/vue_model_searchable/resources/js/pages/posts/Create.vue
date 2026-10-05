<!-- `GET /posts/create` (`create` in `app/controllers/posts.rs`): the form that `store` saves. -->
<script setup lang="ts">
import { Head, Link, useForm } from '@inertiajs/vue3';

const form = useForm({
    title: '',
    body: '',
    user_id: '',
});

function submit() {
    // Number fields hold the text of their input; the server reads numbers (an empty field is a missing value).
    form.transform((data) => ({
        ...data,
        user_id: data.user_id === '' ? '' : Number(data.user_id),
    }));
    form.post('/posts');
}
</script>

<template>
    <main class="mx-auto max-w-3xl px-4 py-12 sm:px-6">
        <Head title="New post" />
        <h1 class="text-3xl font-bold tracking-tight">New post</h1>
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
                <label for="user_id" class="form-label">User id</label>
                <input
                    id="user_id"
                    v-model="form.user_id"
                    name="user_id"
                    type="number"
                    class="form-input"
                    :aria-invalid="form.errors.user_id ? true : undefined"
                    :aria-describedby="form.errors.user_id ? 'user_id-error' : undefined"
                />
                <p v-if="form.errors.user_id" id="user_id-error" class="form-error">{{ form.errors.user_id }}</p>
            </div>
            <div class="flex items-center gap-3">
                <button type="submit" class="btn-primary" :disabled="form.processing">Save</button>
                <Link href="/posts" class="btn-secondary">Cancel</Link>
            </div>
        </form>
    </main>
</template>
