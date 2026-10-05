import { Head } from '@inertiajs/react';

/** `GET /about-us` (`show` in `app/controllers/about_us.rs`). */
export default function AboutUs() {
    return (
        <main className="mx-auto max-w-3xl px-4 py-12 sm:px-6">
            <Head title="About us" />
            <h1 className="text-3xl font-bold tracking-tight">About us</h1>
            <p className="mt-4 text-stone-700 dark:text-stone-300">
                This page is <code className="font-mono text-sm">resources/js/pages/about-us.tsx</code>.
            </p>
        </main>
    );
}
