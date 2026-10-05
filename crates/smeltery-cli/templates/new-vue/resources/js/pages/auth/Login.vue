<script setup lang="ts">
import { Link, useForm } from '@inertiajs/vue3';

import TextInput from '@/components/TextInput.vue';
import AuthLayout from '@/layouts/AuthLayout.vue';

const form = useForm({ email: '', password: '', remember: false });

function submit() {
    form.post('/login', { onFinish: () => form.reset('password') });
}
</script>

<template>
    <AuthLayout title="Log in" description="Welcome back to the forge.">
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <TextInput v-model="form.email" name="email" label="Email" type="email" :error="form.errors.email" required autofocus autocomplete="username" />
            <TextInput v-model="form.password" name="password" label="Password" type="password" :error="form.errors.password" required autocomplete="current-password" />
            <label class="flex items-center gap-2 text-sm text-stone-700 dark:text-stone-300">
                <input
                    v-model="form.remember"
                    type="checkbox"
                    name="remember"
                    class="h-4 w-4 rounded border-ash-200 accent-molten-700 focus-visible:outline-2 focus-visible:outline-molten-500"
                />
                Remember me
            </label>
            <button type="submit" class="btn-primary w-full" :disabled="form.processing">Log in</button>
            <p class="flex justify-between text-sm">
                <Link href="/forgot-password" class="link">Forgot your password?</Link>
                <Link href="/register" class="link">Register</Link>
            </p>
        </form>
    </AuthLayout>
</template>
