import type { RequestEvent } from "./api";

export type RequestOutcomeFilter = "all" | "success" | "failure";

export function successRate(successes: number, requests: number) {
  return requests === 0 ? null : (successes / requests) * 100;
}

export function filterRequests(events: RequestEvent[], outcome: RequestOutcomeFilter, search: string) {
  const needle = search.trim().toLocaleLowerCase();
  return events.filter(event => {
    const outcomeMatches = outcome === "all"
      || (outcome === "success" ? event.outcome === "success" : event.outcome !== "success");
    if (!outcomeMatches) return false;
    if (!needle) return true;
    return [
      event.request_id,
      event.client_key_name,
      event.client_key_prefix,
      event.provider,
      event.error_category,
    ].some(value => value?.toLocaleLowerCase().includes(needle));
  });
}

export function providerKindLabel(kind: string) {
  return kind === "tavily_hikari" ? "Tavily Hikari" : "Searchix";
}
