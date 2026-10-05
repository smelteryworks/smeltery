<script setup lang="ts">
import { Link, useForm } from '@inertiajs/vue3';

import TextInput from '@/components/TextInput.vue';
import AuthLayout from '@/layouts/AuthLayout.vue';

const form = useForm({ name: '', email: '', password: '', password_confirmation: '' });

function submit() {
    form.post('/register', { onFinish: () => form.reset('password', 'password_confirmation') });
}
</script>

<template>
    <AuthLayout title="Create an account" description="One account for everything in this app.">
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <TextInput v-model="form.name" name="name" label="Name" :error="form.errors.name" required autofocus autocomplete="name" />
            <TextInput v-model="form.email" name="email" label="Email" type="email" :error="form.errors.email" required autocomplete="username" />
            <TextInput v-model="form.password" name="password" label="Password" type="password" :error="form.errors.password" required autocomplete="new-password" />
            <TextInput
                v-model="form.password_confirmation"
                name="password_confirmation"
                label="Confirm password"
                type="password"
                :error="form.errors.password_confirmation"
                required
                autocomplete="new-password"
            />
            <button type="submit" class="btn-primary w-full" :disabled="form.processing">Register</button>
            <p class="text-center text-sm text-stone-600 dark:text-stone-400">
                Already registered? <Link href="/login" class="link">Log in</Link>
            </p>
        </form>
    </AuthLayout>
</template>
