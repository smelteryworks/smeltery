import type { InputHTMLAttributes } from 'react';

import InputError from '@/components/input-error';

type Props = InputHTMLAttributes<HTMLInputElement> & {
    /** The field name, also its `id`. */
    name: string;
    label: string;
    /** The validation message for this field, if any. */
    error?: string;
};

/** A labelled form field; with an error it is marked `aria-invalid` and described by the message under it. */
export default function TextInput({ name, label, error, ...input }: Props) {
    return (
        <div>
            <label htmlFor={name} className="form-label">
                {label}
            </label>
            <input
                id={name}
                name={name}
                className="form-input"
                aria-invalid={error ? true : undefined}
                aria-describedby={error ? `${name}-error` : undefined}
                {...input}
            />
            <InputError id={`${name}-error`} message={error} />
        </div>
    );
}
