<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { Check, GitMerge, RefreshCw, ShieldCheck, X } from "@lucide/svelte";
  import { api, type CanonicalBackfillProgress, type CanonicalIdentityRepairPlan } from "../lib/api";

  type MatchCandidate = {
    id: string;
    kind: "release" | "artist";
    leftId: string;
    rightId: string;
    score: number;
    status: string;
    evidence: Record<string, unknown>;
    left: Record<string, unknown> | null;
    right: Record<string, unknown> | null;
    createdAt: string;
    updatedAt: string;
  };

  let working = $state<string | null>(null);
  let failure = $state("");
  const matches = createQuery({
    queryKey: ["match-candidates", "pending"],
    queryFn: () => api<MatchCandidate[]>("/api/v1/matches?status=pending&scope=library")
  });
  const canonical = createQuery({
    queryKey: ["canonical-index"],
    queryFn: () => api<CanonicalBackfillProgress>("/api/v1/index/canonical")
  });

  async function auditIdentities() {
    working = "audit";
    failure = "";
    try {
      await api<CanonicalIdentityRepairPlan>("/api/v1/index/canonical/audit", { method: "POST" });
      await $canonical.refetch();
    } catch (error) {
      failure = error instanceof Error ? error.message : "Could not audit canonical identities";
    } finally {
      working = null;
    }
  }

  async function applyIdentityRepair() {
    const fingerprint = $canonical.data?.identityRepair?.fingerprint;
    if (!fingerprint) return;
    working = "repair";
    failure = "";
    try {
      await api<{ jobId: string }>("/api/v1/index/canonical/repair", {
        method: "POST",
        body: JSON.stringify({ fingerprint })
      });
      await $canonical.refetch();
    } catch (error) {
      failure = error instanceof Error ? error.message : "Could not start canonical identity repair";
    } finally {
      working = null;
    }
  }

  function evidenceLabel(candidate: MatchCandidate): string {
    const evidence = candidate.evidence;
    return String(evidence.title ?? evidence.name ?? "Unlabelled candidate");
  }

  function sideLabel(side: Record<string, unknown> | null): string {
    if (!side) return "Missing canonical record";
    const title = String(side.title ?? side.name ?? "Unlabelled");
    const artist = side.artist ? ` — ${String(side.artist)}` : "";
    const year = side.year ? ` (${String(side.year)})` : "";
    return `${title}${artist}${year}`;
  }

  async function decide(candidate: MatchCandidate, decision: "accept" | "reject") {
    working = candidate.id;
    failure = "";
    try {
      await api<unknown>(
        `/api/v1/matches/${encodeURIComponent(candidate.id)}/${decision}`,
        { method: "POST" }
      );
      await $matches.refetch();
    } catch (error) {
      failure = error instanceof Error ? error.message : "Could not update this match";
    } finally {
      working = null;
    }
  }
</script>

<svelte:head><title>Match review · Wotbox</title></svelte:head>

<header class="page-header">
  <div>
    <p class="eyebrow">Canonical metadata</p>
    <h1>Match review</h1>
    <p>Borderline matches affecting your Library wait here instead of being merged silently.</p>
  </div>
</header>

{#if failure}<div class="error-banner">{failure}</div>{/if}

<section class="panel">
  <div class="section-heading">
    <div>
      <p class="eyebrow">Library identity repair</p>
      <h2>Cross-tracker artist audit</h2>
      <p>Only shared-release evidence and unambiguous tracker ID promotions are applied automatically.</p>
    </div>
    <div class="actions">
      <button class="button secondary" disabled={working !== null} onclick={auditIdentities}>
        <RefreshCw size={15} /> Run audit
      </button>
      {#if $canonical.data?.identityRepair?.state === "audit_ready"}
        <button class="button primary" disabled={working !== null} onclick={applyIdentityRepair}>
          <ShieldCheck size={15} /> Apply audited repair
        </button>
      {/if}
    </div>
  </div>
  {#if $canonical.data?.identityRepair?.plan}
    {@const repair = $canonical.data.identityRepair}
    {@const plan = repair.plan!}
    <p>
      {plan.components.length.toLocaleString()} strong-evidence merge groups ·
      {plan.ambiguousNames.toLocaleString()} ambiguous names retained for review ·
      {plan.staleReleaseSnapshots.toLocaleString()} stale release snapshots.
      {#if repair.state === "applying"} Applied {repair.processed} of {repair.total}.{/if}
      {#if repair.state === "complete"} Repair complete.{/if}
    </p>
  {:else}
    <p>No repair audit has been recorded yet.</p>
  {/if}
</section>

{#if $matches.isPending}
  <div class="panel"><div class="skeleton-row"></div><div class="skeleton-row"></div></div>
{:else if $matches.error}
  <div class="error-banner">{$matches.error.message}</div>
{:else if !$matches.data?.length}
  <div class="search-welcome">
    <GitMerge size={34} />
    <h2>No matches need review</h2>
    <p>High-confidence matches are merged automatically; rejected pairs stay rejected.</p>
  </div>
{:else}
  <section class="panel">
    <div class="match-list">
      {#each $matches.data as candidate}
        <article class="activity-row">
          <div class="activity-copy">
            <strong>{evidenceLabel(candidate)}</strong>
            <span>{candidate.kind} · {(candidate.score * 100).toFixed(1)}% confidence</span>
            <small>{sideLabel(candidate.left)}</small>
            <small>{sideLabel(candidate.right)}</small>
          </div>
          <div class="actions">
            <button
              class="button secondary"
              disabled={working === candidate.id}
              onclick={() => decide(candidate, "reject")}
            ><X size={15} /> Keep separate</button>
            <button
              class="button primary"
              disabled={working === candidate.id}
              onclick={() => decide(candidate, "accept")}
            ><Check size={15} /> Merge</button>
          </div>
        </article>
      {/each}
    </div>
  </section>
{/if}
