<!-- Asked before a page behind `password.confirm`; afterwards the browser goes on to that page. -->
<script setup lang="ts">
import { useForm } from '@inertiajs/vue3';

import TextInput from '@/components/TextInput.vue';
import AuthLayout from '@/layouts/AuthLayout.vue';

const form = useForm({ password: '' });

function submit() {
    form.post('/user/confirm-password', { onFinish: () => form.reset('password') });
}
</script>

<template>
    <AuthLayout title="Confirm your password" description="This part of the app asks for your password again before it goes on.">
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <TextInput v-model="form.password" name="password" label="Password" type="password" :error="form.errors.password" required autofocus autocomplete="current-password" />
            <button type="submit" class="btn-primary w-full" :disabled="form.processing">Confirm</button>
        </form>
    </AuthLayout>
</template>
