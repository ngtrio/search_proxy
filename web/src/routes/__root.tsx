import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { HeadContent, Scripts, createRootRoute } from "@tanstack/react-router";
import { useState } from "react";
import { RootLayout } from "../main";
import { THEME_BOOTSTRAP_SCRIPT, themeBackgrounds } from "../theme";
import "../styles.css";

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: "utf-8" },
      { name: "viewport", content: "width=device-width, initial-scale=1" },
      { title: "Search Proxy | 请求监控" },
      { name: "theme-color", content: themeBackgrounds.light, media: "(prefers-color-scheme: light)" },
      { name: "theme-color", content: themeBackgrounds.dark, media: "(prefers-color-scheme: dark)" },
    ],
  }),
  component: RootDocument,
});

function RootDocument() {
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            staleTime: 60_000,
            retry: 1,
          },
        },
      }),
  );

  return (
    <html lang="zh-CN" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: THEME_BOOTSTRAP_SCRIPT }} />
        <HeadContent />
      </head>
      <body>
        <QueryClientProvider client={client}>
          <RootLayout />
        </QueryClientProvider>
        <Scripts />
      </body>
    </html>
  );
}
