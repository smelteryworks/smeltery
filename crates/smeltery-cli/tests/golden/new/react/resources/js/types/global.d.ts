import '@inertiajs/core';

/** The shared props of `app/providers/alloy.rs`, on every page (`usePage().props`). */
export interface SharedData {
    app: { name: string };
    auth: { user: User | null };
}

/** `SharedUser` in `app/providers/alloy.rs`. */
export interface User {
    id: number;
    name: string;
    email: string;
    email_verified: boolean;
    /** Logging in asks for a two-factor code. */
    two_factor_enabled: boolean;
}

declare module '@inertiajs/core' {
    export interface InertiaConfig {
        sharedPageProps: SharedData;
        /** Values flashed with `session.flash(key, value)` (keys without `_`), as `usePage().flash`. */
        flashDataType: { status?: string; error?: string };
    }
}
