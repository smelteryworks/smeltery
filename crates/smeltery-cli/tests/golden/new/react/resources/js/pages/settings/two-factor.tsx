import { router, useForm } from '@inertiajs/react';
import { useEffect, useState } from 'react';
import type { FormEvent } from 'react';

import TextInput from '@/components/text-input';
import SettingsLayout from '@/layouts/settings-layout';

/** `TwoFactorState` in `app/controllers/settings.rs`. */
interface TwoFactorState {
    enabled: boolean;
    confirmed: boolean;
    recovery_codes_left: number;
}

/** What an authenticator app needs while enrolling: the QR code (an SVG data URL) and the key for typing in. */
interface Setup {
    qrCodeUrl: string;
    secretKey: string;
}

/** Fetches a JSON route of Temper with the session cookie. */
async function json(url: string): Promise<Record<string, unknown>> {
    const response = await fetch(url, { headers: { Accept: 'application/json' } });
    if (!response.ok) {
        throw new Error(`${url}: ${response.status}`);
    }
    return (await response.json()) as Record<string, unknown>;
}

/**
 * Turn two-factor authentication on (scan the QR code, confirm with a first code), see the recovery codes once,
 * make new ones, or turn it off. The QR code and key are fetched from `/user/two-factor-qr-code` and
 * `/user/two-factor-secret-key` while enrolling, so they never sit in the page's props or the browser's history. The
 * recovery codes arrive once as a prop (on the first visit or after a form post); the page keeps its own copy and drops
 * them from the props (and so from the history entry) at once, so the back button does not show them again.
 */
export default function TwoFactor({ two_factor, recovery_codes }: { two_factor: TwoFactorState; recovery_codes: string[] }) {
    const enrolling = two_factor.enabled && !two_factor.confirmed;
    const [setup, setSetup] = useState<Setup | null>(null);
    const enable = useForm({});
    const confirm = useForm({ code: '' });
    const regenerate = useForm({});
    const disable = useForm({});
    const [codes, setCodes] = useState<string[]>([]);

    // Inertia keeps this component across the page's own form posts ("Turn on", "New recovery codes"), so new codes
    // arrive as a changed prop, not a new mount: take every non-empty set, then drop it from the props and the
    // history entry (the empty prop that follows changes nothing here).
    useEffect(() => {
        if (recovery_codes.length > 0) {
            setCodes(recovery_codes);
            router.replaceProp('recovery_codes', []);
        }
    }, [recovery_codes]);

    // Turned off: the old codes no longer work, so they leave the page.
    useEffect(() => {
        if (!two_factor.enabled) {
            setCodes([]);
        }
    }, [two_factor.enabled]);

    useEffect(() => {
        setSetup(null);
        if (!enrolling) {
            return;
        }
        let current = true;
        Promise.all([json('/user/two-factor-qr-code'), json('/user/two-factor-secret-key')])
            .then(([qr, key]) => {
                const url = String(qr.url ?? '');
                // Only the image Temper draws: never another kind of URL in `src`.
                if (current && url.startsWith('data:image/svg+xml;base64,')) {
                    setSetup({ qrCodeUrl: url, secretKey: String(key.secretKey ?? '') });
                }
            })
            .catch(() => {
                if (current) {
                    setSetup(null);
                }
            });
        return () => {
            current = false;
        };
    }, [enrolling]);

    function submitConfirm(event: FormEvent) {
        event.preventDefault();
        confirm.post('/user/confirmed-two-factor-authentication', {
            preserveScroll: true,
            onFinish: () => confirm.reset(),
        });
    }

    return (
        <SettingsLayout title="Two-factor authentication" current="/settings/two-factor">
            <section className="panel mt-8 space-y-5 text-sm text-stone-700 dark:text-stone-300">
                {two_factor.confirmed && (
                    <p>
                        <strong>On.</strong> Logging in asks for a code from your authenticator app. Recovery codes left:{' '}
                        <span className="tabular-nums">{two_factor.recovery_codes_left}</span>.
                    </p>
                )}
                {enrolling && (
                    <>
                        <p>Scan the QR code with your authenticator app (or type the key), then enter the code it shows to finish.</p>
                        {setup && (
                            <>
                                <img src={setup.qrCodeUrl} alt="QR code for your authenticator app" className="w-48 h-48 rounded-lg bg-white" />
                                <p>
                                    Key: <code className="font-mono break-all">{setup.secretKey}</code>
                                </p>
                            </>
                        )}
                        <form onSubmit={submitConfirm} className="space-y-5">
                            <TextInput
                                name="code"
                                label="Code"
                                inputMode="numeric"
                                pattern="[0-9]*"
                                maxLength={6}
                                value={confirm.data.code}
                                onChange={(e) => confirm.setData('code', e.target.value)}
                                error={confirm.errors.code}
                                required
                                autoComplete="one-time-code"
                            />
                            <button type="submit" className="btn-primary" disabled={confirm.processing}>
                                Confirm
                            </button>
                        </form>
                    </>
                )}
                {!two_factor.enabled && (
                    <>
                        <p>
                            <strong>Off.</strong> With two-factor authentication on, logging in also asks for a code from an authenticator app on
                            your phone.
                        </p>
                        <button
                            type="button"
                            className="btn-primary"
                            disabled={enable.processing}
                            onClick={() => enable.post('/user/two-factor-authentication', { preserveScroll: true })}
                        >
                            Turn on
                        </button>
                    </>
                )}
                {codes.length > 0 && (
                    <div className="space-y-3">
                        <p>
                            <strong>Recovery codes.</strong> Store them somewhere safe: each one logs you in once without the app, and this page
                            shows them only now.
                        </p>
                        <ul className="grid gap-2 font-mono sm:grid-cols-2">
                            {codes.map((code) => (
                                <li key={code}>{code}</li>
                            ))}
                        </ul>
                    </div>
                )}
                {two_factor.confirmed && (
                    <button
                        type="button"
                        className="btn-secondary"
                        disabled={regenerate.processing}
                        onClick={() => regenerate.post('/user/two-factor-recovery-codes', { preserveScroll: true })}
                    >
                        New recovery codes
                    </button>
                )}
                {two_factor.enabled && (
                    <p>
                        <button
                            type="button"
                            className="btn-secondary"
                            disabled={disable.processing}
                            onClick={() => disable.delete('/user/two-factor-authentication', { preserveScroll: true })}
                        >
                            Turn off
                        </button>
                    </p>
                )}
            </section>
        </SettingsLayout>
    );
}
