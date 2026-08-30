import { createFileRoute } from "@tanstack/react-router";
import { Providers } from "../main";

export const Route = createFileRoute("/providers")({
  component: Providers,
});
