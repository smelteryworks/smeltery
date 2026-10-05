import { Link, useForm, usePage } from '@inertiajs/react';
import type { FormEvent } from 'react';

import InputError from '@/components/input-error';
import AuthLayout from '@/layouts/auth-layout';

export default function VerifyEmail() {
    const form = useForm({});
    // The resend limit's message (six links a minute) comes back as the `email` error.
    const error = usePage().props.errors.email;

    function submit(event: FormEvent) {
        event.preventDefault();
        form.post('/email/verification-notification');
    }

    return (
        <AuthLayout
            title="Verify your e-mail address"
            description="Thanks for signing up. Open the link in the mail we sent you to verify your address, in this browser while you are logged in. A link opened while logged out leads to the login page: log in, then open the link again."
        >
            <form onSubmit={submit} className="panel mt-8 space-y-5">
                <p className="text-sm text-stone-700 dark:text-stone-300">No mail? Check the spam folder, or send the link again.</p>
                <InputError id="email-error" message={error} />
                <button
                    type="submit"
                    className="btn-primary w-full"
                    disabled={form.processing}
                    aria-describedby={error ? 'email-error' : undefined}
                >
                    Send the link again
                </button>
            </form>
            <p className="mt-6 text-center">
                <Link href="/logout" method="post" as="button" className="link text-sm">
                    Log out
                </Link>
            </p>
        </AuthLayout>
    );
}
