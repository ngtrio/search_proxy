import { createFileRoute } from "@tanstack/react-router";
import { AdminRequests } from "../admin";

export const Route = createFileRoute("/admin/requests")({
  component: AdminRequests,
});
