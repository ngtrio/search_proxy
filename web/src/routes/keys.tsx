import { createFileRoute } from "@tanstack/react-router";
import { Keys } from "../main";

export const Route = createFileRoute("/keys")({
  component: Keys,
});
