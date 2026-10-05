import { Link, useForm } from '@inertiajs/react';
import type { FormEvent } from 'react';

import TextInput from '@/components/text-input';
import AuthLayout from '@/layouts/auth-layout';

export default function ForgotPassword() {
    const form = useForm({ email: '' });

    function submit(event: FormEvent) {
        event.preventDefault();
        form.post('/forgot-password');
    }

    return (
        <AuthLayout title="Forgot your password?" description="Enter your e-mail address and a link to choose a new password is mailed to it.">
            <form onSubmit={submit} className="panel mt-8 space-y-5">
                <TextInput
                    name="email"
                    label="Email"
                    type="email"
                    value={form.data.email}
                    onChange={(e) => form.setData('email', e.target.value)}
                    error={form.errors.email}
                    required
                    autoFocus
                    autoComplete="username"
                />
                <button type="submit" className="btn-primary w-full" disabled={form.processing}>
                    Send the reset link
                </button>
                <p className="text-center text-sm">
                    <Link href="/login" className="link">
                        Back to log in
                    </Link>
                </p>
            </form>
        </AuthLayout>
    );
}
