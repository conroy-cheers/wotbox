<script lang="ts">
  import { QueryClient, QueryClientProvider } from "@tanstack/svelte-query";
  import { onMount } from "svelte";
  import page from "page";
  import { ArrowDownToLine, BookOpen, GitMerge, LayoutDashboard, Radio, Search, Settings2 } from "@lucide/svelte";
  import { appPath, basePath } from "./lib/api";
  import ProviderBanner from "./lib/ProviderBanner.svelte";
  import LiveUpdates from "./lib/LiveUpdates.svelte";
  import Dashboard from "./pages/Dashboard.svelte";
  import Downloads from "./pages/Downloads.svelte";
  import Channels from "./pages/Channels.svelte";
  import ChannelPack from "./pages/ChannelPack.svelte";
  import Library from "./pages/Library.svelte";
  import LibraryArtist from "./pages/LibraryArtist.svelte";
  import Matches from "./pages/Matches.svelte";
  import NotFound from "./pages/NotFound.svelte";
  import Preferences from "./pages/Preferences.svelte";
  import Release from "./pages/Release.svelte";
  import SearchPage from "./pages/Search.svelte";

  type Route =
    | { name: "loading"; key: string }
    | { name: "dashboard"; key: string }
    | { name: "search"; key: string }
    | { name: "library"; key: string }
    | { name: "libraryArtist"; id: string; key: string }
    | { name: "downloads"; key: string }
    | { name: "channels"; key: string }
    | { name: "channelPack"; channel: string; id: string; key: string }
    | { name: "preferences"; key: string }
    | { name: "matches"; key: string }
    | { name: "release"; id: string; source: string; key: string }
    | { name: "notFound"; path: string; key: string };

  const client = new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: 20_000,
        retry: 1,
        refetchOnWindowFocus: false
      }
    }
  });
  let route = $state<Route>({ name: "loading", key: "loading" });

  function routeKey(context: PageJS.Context): string {
    return context.path.split("#", 1)[0];
  }

  onMount(() => {
    page.base(basePath);
    page("/", (context) => route = { name: "dashboard", key: routeKey(context) });
    page("/search", (context) => route = { name: "search", key: routeKey(context) });
    page("/library", (context) => route = { name: "library", key: routeKey(context) });
    page("/library/artists/:id", (context) => route = {
      name: "libraryArtist",
      id: context.params.id,
      key: routeKey(context)
    });
    page("/downloads", (context) => route = { name: "downloads", key: routeKey(context) });
    page("/channels", (context) => route = { name: "channels", key: routeKey(context) });
    page("/channels/:channel/packs/:id", (context) => route = {
      name: "channelPack",
      channel: context.params.channel,
      id: context.params.id,
      key: routeKey(context)
    });
    page("/preferences", (context) => route = { name: "preferences", key: routeKey(context) });
    page("/matches", (context) => route = { name: "matches", key: routeKey(context) });
    page("/releases/:id", (context) => route = {
      name: "release",
      id: context.params.id,
      source: new URLSearchParams(context.querystring).get("from") ?? "search",
      key: routeKey(context)
    });
    page("*", (context) => route = {
      name: "notFound",
      path: context.pathname,
      key: routeKey(context)
    });
    page.start();
    return () => page.stop();
  });

  const navigation = [
    { name: "dashboard", label: "Dashboard", path: "/", icon: LayoutDashboard },
    { name: "search", label: "Search", path: "/search", icon: Search },
    { name: "library", label: "Library", path: "/library", icon: BookOpen },
    { name: "downloads", label: "Downloads", path: "/downloads", icon: ArrowDownToLine },
    { name: "channels", label: "Channels", path: "/channels", icon: Radio },
    { name: "matches", label: "Match review", path: "/matches", icon: GitMerge },
    { name: "preferences", label: "Preferences", path: "/preferences", icon: Settings2 }
  ];

  function isActive(name: string): boolean {
    if (route.name === name) return true;
    if (name === "library") {
      return route.name === "libraryArtist"
        || (route.name === "release" && route.source === "library");
    }
    if (name === "downloads") {
      return route.name === "release" && route.source === "downloads";
    }
    if (name === "channels") {
      return route.name === "channelPack"
        || (route.name === "release" && route.source === "channels");
    }
    return name === "search"
      && route.name === "release"
      && !["library", "downloads"].includes(route.source);
  }
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
          <a href={appPath(item.path)} class:active={isActive(item.name)}>
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
      <LiveUpdates />
      <ProviderBanner />
      {#key route.key}
        {#if route.name === "dashboard"}
          <Dashboard />
        {:else if route.name === "search"}
          <SearchPage />
        {:else if route.name === "library"}
          <Library />
        {:else if route.name === "libraryArtist"}
          <LibraryArtist id={route.id} />
        {:else if route.name === "downloads"}
          <Downloads />
        {:else if route.name === "channels"}
          <Channels />
        {:else if route.name === "channelPack"}
          <ChannelPack id={route.id} />
        {:else if route.name === "preferences"}
          <Preferences />
        {:else if route.name === "matches"}
          <Matches />
        {:else if route.name === "release"}
          <Release id={route.id} />
        {:else if route.name === "notFound"}
          <NotFound path={route.path} />
        {/if}
      {/key}
    </main>
    <nav class="mobile-nav" aria-label="Mobile navigation">
      {#each navigation as item}
        <a href={appPath(item.path)} class:active={isActive(item.name)}>
          <item.icon size={19} />
          <span>{item.label}</span>
        </a>
      {/each}
    </nav>
  </div>
</QueryClientProvider>
