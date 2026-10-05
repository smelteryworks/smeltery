import { useForm } from '@inertiajs/react';
import type { FormEvent } from 'react';

import TextInput from '@/components/text-input';
import AuthLayout from '@/layouts/auth-layout';

/** Asked before a page behind `password.confirm`; afterwards the browser goes on to that page. */
export default function ConfirmPassword() {
    const form = useForm({ password: '' });

    function submit(event: FormEvent) {
        event.preventDefault();
        form.post('/user/confirm-password', { onFinish: () => form.reset('password') });
    }

    return (
        <AuthLayout title="Confirm your password" description="This part of the app asks for your password again before it goes on.">
            <form onSubmit={submit} className="panel mt-8 space-y-5">
                <TextInput
                    name="password"
                    label="Password"
                    type="password"
                    value={form.data.password}
                    onChange={(e) => form.setData('password', e.target.value)}
                    error={form.errors.password}
                    required
                    autoFocus
                    autoComplete="current-password"
                />
                <button type="submit" className="btn-primary w-full" disabled={form.processing}>
                    Confirm
                </button>
            </form>
        </AuthLayout>
    );
}
