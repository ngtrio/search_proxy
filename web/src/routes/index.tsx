import { createFileRoute } from "@tanstack/react-router";
import { Overview } from "../main";

export const Route = createFileRoute("/")({
  component: Overview,
});
