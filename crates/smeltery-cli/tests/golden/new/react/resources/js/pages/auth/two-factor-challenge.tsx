import { useForm } from '@inertiajs/react';
import { useState } from 'react';
import type { FormEvent } from 'react';

import TextInput from '@/components/text-input';
import AuthLayout from '@/layouts/auth-layout';

/** The second step of a login with two-factor authentication: a code from the app, or a recovery code. */
export default function TwoFactorChallenge() {
    const [recovery, setRecovery] = useState(false);
    const form = useForm({ code: '', recovery_code: '' });

    function submit(event: FormEvent) {
        event.preventDefault();
        form.transform((data) => (recovery ? { recovery_code: data.recovery_code } : { code: data.code }));
        form.post('/two-factor-challenge', { onFinish: () => form.reset() });
    }

    function toggle() {
        setRecovery(!recovery);
        form.clearErrors();
        form.reset();
    }

    return (
        <AuthLayout
            title="Two-factor authentication"
            description={recovery ? 'Enter one of your recovery codes.' : 'Enter the code from your authenticator app.'}
        >
            <form onSubmit={submit} className="panel mt-8 space-y-5">
                {recovery ? (
                    <TextInput
                        key="recovery_code"
                        name="recovery_code"
                        label="Recovery code"
                        value={form.data.recovery_code}
                        onChange={(e) => form.setData('recovery_code', e.target.value)}
                        error={form.errors.recovery_code}
                        required
                        autoFocus
                        autoComplete="off"
                    />
                ) : (
                    <TextInput
                        key="code"
                        name="code"
                        label="Code"
                        inputMode="numeric"
                        pattern="[0-9]*"
                        maxLength={6}
                        value={form.data.code}
                        onChange={(e) => form.setData('code', e.target.value)}
                        error={form.errors.code}
                        required
                        autoFocus
                        autoComplete="one-time-code"
                    />
                )}
                <button type="submit" className="btn-primary w-full" disabled={form.processing}>
                    Log in
                </button>
                <p className="text-sm">
                    <button type="button" className="link" onClick={toggle}>
                        {recovery ? 'Use a code from the app instead' : 'Use a recovery code instead'}
                    </button>
                </p>
            </form>
        </AuthLayout>
    );
}
