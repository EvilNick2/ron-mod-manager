import { store, type DialogRequest } from "./store";

let nextId = 0;

function enqueue(request: Omit<DialogRequest, "id" | "resolve">): Promise<boolean | string | null> {
  return new Promise((resolve) => {
    store.dialogs.push({ ...request, id: ++nextId, resolve });
  });
}

const base = { title: "", value: "", confirmLabel: "OK", cancelLabel: "Cancel", danger: false };

export async function showAlert(message: string, title?: string): Promise<void> {
  await enqueue({ ...base, kind: "alert", message, title: title ?? "" });
}

export async function showConfirm(
  message: string,
  opts: { title?: string; confirmLabel?: string; cancelLabel?: string; danger?: boolean } = {},
): Promise<boolean> {
  const result = await enqueue({
    ...base,
    kind: "confirm",
    message,
    title: opts.title ?? "",
    confirmLabel: opts.confirmLabel ?? "OK",
    cancelLabel: opts.cancelLabel ?? "Cancel",
    danger: opts.danger ?? false,
  });
  return result === true;
}

export async function showPrompt(
  message: string,
  defaultValue?: string,
  opts: { title?: string; confirmLabel?: string; validate?: (v: string) => string | null } = {},
): Promise<string | null> {
  const result = await enqueue({
    ...base,
    kind: "prompt",
    message,
    value: defaultValue ?? "",
    title: opts.title ?? "",
    confirmLabel: opts.confirmLabel ?? "OK",
    validate: opts.validate,
  });
  return typeof result === "string" ? result : null;
}

// Fallback when the clipboard cannot be written, the text is shown selected for a manual copy
export async function showCopy(text: string): Promise<void> {
  await enqueue({ ...base, kind: "copy", title: "Copy this link", message: "", value: text, confirmLabel: "Close" });
}

// Resolves to the picked choice's value, or null when cancelled
export async function showChoice(
  message: string,
  choices: { label: string; value: string }[],
  opts: { title?: string } = {},
): Promise<string | null> {
  const result = await enqueue({ ...base, kind: "choice", message, title: opts.title ?? "", choices });
  return typeof result === "string" ? result : null;
}
