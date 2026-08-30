import { createFileRoute } from "@tanstack/react-router";
import { Requests } from "../main";

export const Route = createFileRoute("/requests")({
  component: Requests,
});
