import { createFileRoute } from "@tanstack/react-router";
import { AdminRouteLayout } from "../admin";

export const Route = createFileRoute("/admin")({
  component: AdminRouteLayout,
});
