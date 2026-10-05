<!-- `GET /photos/create` (`create` in `app/controllers/photos.rs`): the form that `store` saves. -->
<script setup lang="ts">
import { Head, Link, useForm } from '@inertiajs/vue3';

const form = useForm({
    title: '',
    image: null as File | null,
    scan: null as File | null,
    public: false,
});

function submit() {
    // With a file the form goes up as multipart/form-data, where a checkbox is the text `true` or `false`.
    form.transform((data) => ({
        ...data,
        public: String(data.public),
    }));
    form.post('/photos', { forceFormData: true });
}
</script>

<template>
    <main class="mx-auto max-w-3xl px-4 py-12 sm:px-6">
        <Head title="New photo" />
        <h1 class="text-3xl font-bold tracking-tight">New photo</h1>
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
                <label for="image" class="form-label">Image</label>
                <input
                    id="image"
                    name="image"
                    type="file"
                    required
                    class="form-input"
                    :aria-invalid="form.errors.image ? true : undefined"
                    :aria-describedby="form.errors.image ? 'image-error' : undefined"
                    @input="form.image = ($event.target as HTMLInputElement).files?.[0] ?? null"
                />
                <p v-if="form.errors.image" id="image-error" class="form-error">{{ form.errors.image }}</p>
            </div>
            <div>
                <label for="scan" class="form-label">Scan</label>
                <input
                    id="scan"
                    name="scan"
                    type="file"
                    class="form-input"
                    :aria-invalid="form.errors.scan ? true : undefined"
                    :aria-describedby="form.errors.scan ? 'scan-error' : undefined"
                    @input="form.scan = ($event.target as HTMLInputElement).files?.[0] ?? null"
                />
                <p v-if="form.errors.scan" id="scan-error" class="form-error">{{ form.errors.scan }}</p>
            </div>
            <div>
                <label for="public" class="form-label">Public</label>
                <input
                    id="public"
                    v-model="form.public"
                    name="public"
                    type="checkbox"
                    class="mt-2 h-4 w-4 accent-molten-700"
                    :aria-invalid="form.errors.public ? true : undefined"
                    :aria-describedby="form.errors.public ? 'public-error' : undefined"
                />
                <p v-if="form.errors.public" id="public-error" class="form-error">{{ form.errors.public }}</p>
            </div>
            <div class="flex items-center gap-3">
                <button type="submit" class="btn-primary" :disabled="form.processing">Save</button>
                <Link href="/photos" class="btn-secondary">Cancel</Link>
            </div>
        </form>
    </main>
</template>
