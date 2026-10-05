/** A field's validation message (`form.errors.email`), linked to the field by `id` (`<field>-error`). */
export default function InputError({ id, message }: { id: string; message?: string }) {
    if (!message) {
        return null;
    }
    return (
        <p id={id} className="form-error">
            {message}
        </p>
    );
}
