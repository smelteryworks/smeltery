import { Head } from '@inertiajs/react';
import type { ReactNode } from 'react';

/** The narrow, centred column of the authentication pages: a title, a line under it and the form. */
export default function AuthLayout({ title, description, children }: { title: string; description: string; children: ReactNode }) {
    return (
        <main className="mx-auto max-w-md px-4 py-12 sm:py-16">
            <Head title={title} />
            <h1 className="text-center text-2xl font-bold tracking-tight">{title}</h1>
            <p className="mt-2 text-center text-sm text-stone-600 dark:text-stone-400">{description}</p>
            {children}
        </main>
    );
}
