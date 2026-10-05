<!-- The page of a reset link: `token` from its path, `email` from its query string. -->
<script setup lang="ts">
import { useForm } from '@inertiajs/vue3';

import TextInput from '@/components/TextInput.vue';
import AuthLayout from '@/layouts/AuthLayout.vue';

const props = defineProps<{ token: string; email: string }>();

const form = useForm({ email: props.email, password: '', password_confirmation: '' });

function submit() {
    form.post(`/reset-password/${encodeURIComponent(props.token)}`, {
        onFinish: () => form.reset('password', 'password_confirmation'),
    });
}
</script>

<template>
    <AuthLayout title="Reset password" description="Choose a new password for your account.">
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <TextInput v-model="form.email" name="email" label="Email" type="email" :error="form.errors.email" required autocomplete="username" />
            <TextInput v-model="form.password" name="password" label="New password" type="password" :error="form.errors.password" required autofocus autocomplete="new-password" />
            <TextInput
                v-model="form.password_confirmation"
                name="password_confirmation"
                label="Confirm password"
                type="password"
                :error="form.errors.password_confirmation"
                required
                autocomplete="new-password"
            />
            <button type="submit" class="btn-primary w-full" :disabled="form.processing">Reset password</button>
        </form>
    </AuthLayout>
</template>
