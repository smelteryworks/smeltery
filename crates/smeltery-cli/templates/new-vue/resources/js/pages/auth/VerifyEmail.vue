<script setup lang="ts">
import { Link, useForm, usePage } from '@inertiajs/vue3';
import { computed } from 'vue';

import InputError from '@/components/InputError.vue';
import AuthLayout from '@/layouts/AuthLayout.vue';

const form = useForm({});
// The resend limit's message (six links a minute) comes back as the `email` error.
const page = usePage();
const error = computed(() => page.props.errors.email);

function submit() {
    form.post('/email/verification-notification');
}
</script>

<template>
    <AuthLayout
        title="Verify your e-mail address"
        description="Thanks for signing up. Open the link in the mail we sent you to verify your address, in this browser while you are logged in. A link opened while logged out leads to the login page: log in, then open the link again."
    >
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <p class="text-sm text-stone-700 dark:text-stone-300">No mail? Check the spam folder, or send the link again.</p>
            <InputError id="email-error" :message="error" />
            <button
                type="submit"
                class="btn-primary w-full"
                :disabled="form.processing"
                :aria-describedby="error ? 'email-error' : undefined"
            >
                Send the link again
            </button>
        </form>
        <p class="mt-6 text-center">
            <Link href="/logout" method="post" as="button" class="link text-sm">Log out</Link>
        </p>
    </AuthLayout>
</template>
