import { createFileRoute } from "@tanstack/react-router";
import { AdminLogin } from "../admin";

export const Route = createFileRoute("/admin/login")({
  validateSearch: (search: Record<string, unknown>) => ({
    redirect: typeof search.redirect === "string" ? search.redirect : undefined,
  }),
  component: LoginRoute,
});

function LoginRoute() {
  const { redirect } = Route.useSearch();
  return <AdminLogin redirect={redirect} />;
}
