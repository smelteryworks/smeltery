import { Link, useForm } from '@inertiajs/react';
import type { FormEvent } from 'react';

import TextInput from '@/components/text-input';
import AuthLayout from '@/layouts/auth-layout';

export default function Login() {
    const form = useForm({ email: '', password: '', remember: false });

    function submit(event: FormEvent) {
        event.preventDefault();
        form.post('/login', { onFinish: () => form.reset('password') });
    }

    return (
        <AuthLayout title="Log in" description="Welcome back to the forge.">
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
                <TextInput
                    name="password"
                    label="Password"
                    type="password"
                    value={form.data.password}
                    onChange={(e) => form.setData('password', e.target.value)}
                    error={form.errors.password}
                    required
                    autoComplete="current-password"
                />
                <label className="flex items-center gap-2 text-sm text-stone-700 dark:text-stone-300">
                    <input
                        type="checkbox"
                        name="remember"
                        checked={form.data.remember}
                        onChange={(e) => form.setData('remember', e.target.checked)}
                        className="h-4 w-4 rounded border-ash-200 accent-molten-700 focus-visible:outline-2 focus-visible:outline-molten-500"
                    />
                    Remember me
                </label>
                <button type="submit" className="btn-primary w-full" disabled={form.processing}>
                    Log in
                </button>
                <p className="flex justify-between text-sm">
                    <Link href="/forgot-password" className="link">
                        Forgot your password?
                    </Link>{' '}
                    <Link href="/register" className="link">
                        Register
                    </Link>
                </p>
            </form>
        </AuthLayout>
    );
}
