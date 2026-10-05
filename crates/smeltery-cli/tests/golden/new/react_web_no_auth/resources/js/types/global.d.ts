import '@inertiajs/core';

/** The shared props of `app/providers/alloy.rs`, on every page (`usePage().props`). */
export interface SharedData {
    app: { name: string };
}

declare module '@inertiajs/core' {
    export interface InertiaConfig {
        sharedPageProps: SharedData;
        /** Values flashed with `session.flash(key, value)` (keys without `_`), as `usePage().flash`. */
        flashDataType: { status?: string; error?: string };
    }
}
