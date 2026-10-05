import { useForm } from '@inertiajs/react';
import type { FormEvent } from 'react';

import TextInput from '@/components/text-input';
import AuthLayout from '@/layouts/auth-layout';

/** The page of a reset link: `token` from its path, `email` from its query string. */
export default function ResetPassword({ token, email }: { token: string; email: string }) {
    const form = useForm({ email, password: '', password_confirmation: '' });

    function submit(event: FormEvent) {
        event.preventDefault();
        form.post(`/reset-password/${encodeURIComponent(token)}`, {
            onFinish: () => form.reset('password', 'password_confirmation'),
        });
    }

    return (
        <AuthLayout title="Reset password" description="Choose a new password for your account.">
            <form onSubmit={submit} className="panel mt-8 space-y-5">
                <TextInput
                    name="email"
                    label="Email"
                    type="email"
                    value={form.data.email}
                    onChange={(e) => form.setData('email', e.target.value)}
                    error={form.errors.email}
                    required
                    autoComplete="username"
                />
                <TextInput
                    name="password"
                    label="New password"
                    type="password"
                    value={form.data.password}
                    onChange={(e) => form.setData('password', e.target.value)}
                    error={form.errors.password}
                    required
                    autoFocus
                    autoComplete="new-password"
                />
                <TextInput
                    name="password_confirmation"
                    label="Confirm password"
                    type="password"
                    value={form.data.password_confirmation}
                    onChange={(e) => form.setData('password_confirmation', e.target.value)}
                    error={form.errors.password_confirmation}
                    required
                    autoComplete="new-password"
                />
                <button type="submit" className="btn-primary w-full" disabled={form.processing}>
                    Reset password
                </button>
            </form>
        </AuthLayout>
    );
}
