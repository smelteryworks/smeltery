import { router, usePage } from '@inertiajs/react';
import { useState } from 'react';

import Card from '@/components/card';
import Ingot from '@/components/ingot';

/** `ForgeReading` in `app/controllers/home.rs`. */
interface ForgeReading {
    temperature: number;
    reading: number;
    served_at: string;
}

/**
 * The welcome page. Replace it with your own: the route is in routes/web.rs, the controller in
 * app/controllers/home.rs.
 */
export default function Welcome({ version, forge }: { version: string; forge?: ForgeReading }) {
    const { app } = usePage().props;
    // Browser state: it survives the partial reload below, because only the `forge` prop changes.
    const [asked, setAsked] = useState(0);
    const [asking, setAsking] = useState(false);

    function askTheForge() {
        setAsked((n) => n + 1);
        router.reload({
            only: ['forge'],
            onStart: () => setAsking(true),
            onFinish: () => setAsking(false),
        });
    }

    return (
        <main className="mx-auto max-w-6xl px-4 sm:px-6">
            <section className="grid items-center gap-10 py-14 sm:py-20 lg:grid-cols-[1.15fr_1fr]">
                <div>
                    <p className="inline-flex items-center gap-2 rounded-full border border-ash-200 bg-white/70 px-3 py-1 font-mono text-xs text-stone-700 dark:border-forge-700 dark:bg-forge-900/70 dark:text-stone-300">
                        <span className="h-1.5 w-1.5 rounded-full bg-molten-500 shadow-[0_0_8px_2px_rgb(255_74_28/0.7)]" aria-hidden="true"></span>
                        Rust · Smeltery {version} · React + Inertia
                    </p>
                    <h1 className="mt-6 text-4xl font-bold tracking-tight text-balance sm:text-5xl lg:text-6xl">
                        {app.name} is <span className="text-molten-700 dark:text-molten-500">running</span> on{' '}
                        <span className="bg-linear-to-r from-molten-800 to-molten-600 bg-clip-text text-transparent dark:from-molten-500 dark:to-gold-400">Smeltery</span>
                    </h1>
                    <p className="mt-6 max-w-xl text-lg text-stone-700 dark:text-stone-300">
                        The forge is hot. Rust answers on the server, React draws the pages, and Inertia carries the props between them.
                    </p>
                    <div className="mt-8 flex flex-wrap gap-3">
                        <a href="https://github.com/smelteryworks/smeltery#readme" className="btn-primary">
                            Read the guide
                        </a>
                        <a href="https://docs.rs/smeltery" className="btn-secondary">
                            API docs on docs.rs
                        </a>
                    </div>
                </div>
                <Ingot />
            </section>

            <section aria-labelledby="next-steps" className="pb-16">
                <h2 id="next-steps" className="mono-label">
                    Next steps
                </h2>
                <div className="mt-4 grid gap-4 md:grid-cols-2">
                    <Card title="Edit this page">
                        <p>
                            This page is <code>resources/js/pages/welcome.tsx</code>; its route lives in <code>routes/web.rs</code> and its
                            props come from <code>app/controllers/home.rs</code>. The Vite dev server updates the page as you save.
                        </p>
                    </Card>
                    <Card title="Make a model">
                        <p>
                            Run <code>smeltery make:model Post title:string --all</code> for a model, migration, factory, seeder, controller, pages and routes, then{' '}
                            <code>smeltery migrate</code>.
                        </p>
                    </Card>
                </div>
            </section>

            <section aria-labelledby="forge" className="pb-20">
                <h2 id="forge" className="mono-label">
                    A partial reload
                </h2>
                <div className="panel mt-4 flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
                    <div className="text-sm leading-relaxed text-stone-700 dark:text-stone-300">
                        <h3 className="mb-2 text-base font-semibold text-forge-900 dark:text-ash-100">Ask the forge</h3>
                        <p>
                            The reading is an optional prop: the server computes it only when a partial reload asks for <code className="rounded bg-ash-100 px-1 py-0.5 font-mono text-xs text-forge-900 dark:bg-forge-800 dark:text-gold-300">forge</code>.
                        </p>
                        <p className="mt-2" aria-live="polite">
                            {forge ? (
                                <>
                                    Reading <span className="font-mono">#{forge.reading}</span>:{' '}
                                    <span className="font-mono text-2xl font-bold text-molten-700 dark:text-molten-500">{forge.temperature} °C</span> at{' '}
                                    <span className="font-mono">{forge.served_at}</span> UTC.
                                </>
                            ) : (
                                'No reading yet.'
                            )}
                        </p>
                        <p className="mt-2 text-xs text-stone-600 dark:text-stone-400">
                            Asked {asked} {asked === 1 ? 'time' : 'times'} from this page; the count lives in the browser and survives each reload.
                        </p>
                    </div>
                    <button type="button" className="btn-primary shrink-0" onClick={askTheForge} disabled={asking}>
                        {asking ? 'Reading…' : 'Ask the forge'}
                    </button>
                </div>
            </section>
        </main>
    );
}
