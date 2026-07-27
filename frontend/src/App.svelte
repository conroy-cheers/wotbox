<script lang="ts">
  import { QueryClient, QueryClientProvider } from "@tanstack/svelte-query";
  import { onMount } from "svelte";
  import page from "page";
  import { ArrowDownToLine, LayoutDashboard, Search, Settings2 } from "@lucide/svelte";
  import { appPath, basePath } from "./lib/api";
  import Dashboard from "./pages/Dashboard.svelte";
  import DownloadDetails from "./pages/DownloadDetails.svelte";
  import Downloads from "./pages/Downloads.svelte";
  import Release from "./pages/Release.svelte";
  import SearchPage from "./pages/Search.svelte";

  type Route =
    | { name: "dashboard" }
    | { name: "search" }
    | { name: "downloads" }
    | { name: "download"; client: string; infoHash: string }
    | { name: "release"; tracker: string; id: string };

  const client = new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: 20_000,
        retry: 1,
        refetchOnWindowFocus: false
      }
    }
  });
  let route = $state<Route>({ name: "dashboard" });

  onMount(() => {
    page.base(basePath);
    page("/", () => route = { name: "dashboard" });
    page("/search", () => route = { name: "search" });
    page("/downloads", () => route = { name: "downloads" });
    page("/downloads/:client/:infoHash", (context) => route = {
      name: "download",
      client: context.params.client,
      infoHash: context.params.infoHash
    });
    page("/releases/:tracker/:id", (context) => route = {
      name: "release",
      tracker: context.params.tracker,
      id: context.params.id
    });
    page("*", () => route = { name: "dashboard" });
    page.start();
    return () => page.stop();
  });

  const navigation = [
    { name: "dashboard", label: "Dashboard", path: "/", icon: LayoutDashboard },
    { name: "search", label: "Search", path: "/search", icon: Search },
    { name: "downloads", label: "Downloads", path: "/downloads", icon: ArrowDownToLine }
  ];
</script>

<QueryClientProvider client={client}>
  <div class="app-shell">
    <aside class="sidebar">
      <a class="brand" href={appPath("/")}>
        <span class="brand-mark">W</span>
        <span><strong>Wotbox</strong><small>music manager</small></span>
      </a>
      <nav aria-label="Main navigation">
        {#each navigation as item}
          <a href={appPath(item.path)} class:active={route.name === item.name}>
            <item.icon size={19} />
            <span>{item.label}</span>
          </a>
        {/each}
      </nav>
      <div class="sidebar-foot">
        <Settings2 size={17} />
        <span>Gazelle · qBittorrent</span>
      </div>
    </aside>
    <main>
      {#if route.name === "dashboard"}
        <Dashboard />
      {:else if route.name === "search"}
        <SearchPage />
      {:else if route.name === "downloads"}
        <Downloads />
      {:else if route.name === "download"}
        <DownloadDetails client={route.client} infoHash={route.infoHash} />
      {:else}
        <Release tracker={route.tracker} id={route.id} />
      {/if}
    </main>
    <nav class="mobile-nav" aria-label="Mobile navigation">
      {#each navigation as item}
        <a href={appPath(item.path)} class:active={route.name === item.name}>
          <item.icon size={19} />
          <span>{item.label}</span>
        </a>
      {/each}
    </nav>
  </div>
</QueryClientProvider>
