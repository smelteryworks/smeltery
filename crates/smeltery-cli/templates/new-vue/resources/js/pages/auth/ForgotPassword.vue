<script setup lang="ts">
import { Link, useForm } from '@inertiajs/vue3';

import TextInput from '@/components/TextInput.vue';
import AuthLayout from '@/layouts/AuthLayout.vue';

const form = useForm({ email: '' });

function submit() {
    form.post('/forgot-password');
}
</script>

<template>
    <AuthLayout title="Forgot your password?" description="Enter your e-mail address and a link to choose a new password is mailed to it.">
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <TextInput v-model="form.email" name="email" label="Email" type="email" :error="form.errors.email" required autofocus autocomplete="username" />
            <button type="submit" class="btn-primary w-full" :disabled="form.processing">Send the reset link</button>
            <p class="text-center text-sm"><Link href="/login" class="link">Back to log in</Link></p>
        </form>
    </AuthLayout>
</template>
