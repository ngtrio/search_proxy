import { createFileRoute } from "@tanstack/react-router";
import { AdminKeys } from "../admin";

export const Route = createFileRoute("/admin/keys")({
  component: AdminKeys,
});
