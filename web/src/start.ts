import { createStart } from "@tanstack/react-start";

// The admin console authenticates and loads data in the browser so the existing
// Rust API session cookie remains the source of truth. SSR renders the document
// and loading state; data queries then continue in the browser with the user's
// session cookie.
export const startInstance = createStart(() => ({
  defaultSsr: true,
}));
