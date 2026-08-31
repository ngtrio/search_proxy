import { createFileRoute } from "@tanstack/react-router";
import { AdminProviders } from "../admin";

export const Route = createFileRoute("/admin/providers")({
  component: AdminProviders,
});
