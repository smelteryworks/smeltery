import { Link, useForm } from '@inertiajs/react';
import type { FormEvent } from 'react';

import TextInput from '@/components/text-input';
import AuthLayout from '@/layouts/auth-layout';

export default function Register() {
    const form = useForm({ name: '', email: '', password: '', password_confirmation: '' });

    function submit(event: FormEvent) {
        event.preventDefault();
        form.post('/register', { onFinish: () => form.reset('password', 'password_confirmation') });
    }

    return (
        <AuthLayout title="Create an account" description="One account for everything in this app.">
            <form onSubmit={submit} className="panel mt-8 space-y-5">
                <TextInput
                    name="name"
                    label="Name"
                    value={form.data.name}
                    onChange={(e) => form.setData('name', e.target.value)}
                    error={form.errors.name}
                    required
                    autoFocus
                    autoComplete="name"
                />
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
                    label="Password"
                    type="password"
                    value={form.data.password}
                    onChange={(e) => form.setData('password', e.target.value)}
                    error={form.errors.password}
                    required
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
                    Register
                </button>
                <p className="text-center text-sm text-stone-600 dark:text-stone-400">
                    Already registered?{' '}
                    <Link href="/login" className="link">
                        Log in
                    </Link>
                </p>
            </form>
        </AuthLayout>
    );
}
