import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { FFDBClient, MemoryDeveloperSessionStore, type HostUpdateJob, type HostUpdateStatus } from "@ffdb/client";

import { InstanceUpdatesPanel } from "./InstanceUpdates.js";

describe("instance host updates", () => {
  afterEach(() => {
    cleanup();
    globalThis.sessionStorage.clear();
  });

  it("shows signed release evidence and compatible rollback choices before mutation", async () => {
    const client = await updateClient(async (request) => {
      if (request.url.endsWith("/v1/instance/updates")) return Response.json(updateStatus());
      return missingResponse();
    });

    render(<InstanceUpdatesPanel client={client} onNotice={() => undefined} />);

    expect(await screen.findByText("FFDB 0.3.3 is available")).toBeInTheDocument();
    expect(screen.getByText("FFDB 0.3.1")).toBeInTheDocument();
    expect(screen.getByText(/State schema 1. Eligible for an atomic rollback/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Rollback…" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Review update" }));
    const dialog = screen.getByRole("dialog", { name: "Update to FFDB 0.3.3?" });
    expect(within(dialog).getByText("Verified")).toBeInTheDocument();
    expect(within(dialog).getByText("https://github.com/Forever-Frameworks-LLC/ffdb/.github/workflows/release.yml@refs/tags/v0.3.3")).toBeInTheDocument();
    expect(within(dialog).getByRole("link", { name: /Read release notes/i })).toHaveAttribute("href", "https://github.com/Forever-Frameworks-LLC/ffdb/releases/tag/v0.3.3");

    fireEvent.keyDown(globalThis.document, { key: "Escape" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("reauthenticates through developer sign-in and retries the exact pending install without forwarding the password", async () => {
    const calls: Request[] = [];
    let installed = false;
    let installAttempts = 0;
    const notice = vi.fn();
    const client = await updateClient(async (request) => {
      calls.push(request);
      const path = new URL(request.url).pathname;
      if (path === "/v1/instance/updates" && request.method === "GET") return Response.json(updateStatus(installed));
      if (path === "/v1/instance/updates/install") {
        installAttempts += 1;
        if (installAttempts === 1) return Response.json({ error: { code: "platform_auth.reauthentication_required", message: "recent sign-in required", request_id: "reauth-1" } }, { status: 428 });
        return Response.json(updateJob({ state: "queued", phase: "queued" }), { status: 202 });
      }
      if (path === "/v1/developer/sign-in") return Response.json({ session_token: "fresh-session", user_id: "owner-1", email: "owner@example.test", expires_at_ms: Date.now() + 60_000 });
      if (path === "/v1/instance/updates/jobs/job-1") {
        installed = true;
        return Response.json(updateJob({ state: "succeeded", phase: "ready", installed_version: "0.3.3", message: "Update completed" }));
      }
      return missingResponse();
    });

    const onReleaseChange = vi.fn();
    render(<InstanceUpdatesPanel client={client} onNotice={notice} onReleaseChange={onReleaseChange} />);
    fireEvent.click(await screen.findByRole("button", { name: "Review update" }));
    fireEvent.click(screen.getByRole("button", { name: "Install update" }));
    expect(await screen.findByRole("heading", { name: "Sign in again to continue" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Password"), { target: { value: "correct horse battery staple" } });
    fireEvent.click(screen.getByRole("button", { name: "Sign in and install update" }));

    await waitFor(() => expect(notice).toHaveBeenCalledWith("Updated to FFDB 0.3.3"), { timeout: 4_000 });
    expect(onReleaseChange).toHaveBeenCalledWith("install", "0.3.3");
    expect(installAttempts).toBe(2);
    const installRequests = calls.filter((request) => new URL(request.url).pathname === "/v1/instance/updates/install");
    for (const request of installRequests) {
      expect(request.headers.get("authorization")).toMatch(/^Bearer /u);
      await expect(request.clone().json()).resolves.toEqual({ version: "0.3.3" });
      expect(await request.clone().text()).not.toContain("correct horse battery staple");
    }
    const signIn = calls.find((request) => new URL(request.url).pathname === "/v1/developer/sign-in");
    await expect(signIn?.clone().json()).resolves.toEqual({ email: "owner@example.test", password: "correct horse battery staple" });
  });

  it("renders topology-specific guidance when portal-managed updates are unsupported", async () => {
    const client = await updateClient(async (request) => request.url.endsWith("/v1/instance/updates")
      ? Response.json({ ...updateStatus(), supported: false, unavailable_reason: "Docker hosts keep lifecycle authority outside the API container." })
      : missingResponse());

    render(<InstanceUpdatesPanel client={client} onNotice={() => undefined} />);

    expect(await screen.findByRole("heading", { name: "Portal updates are not available on this installation" })).toBeInTheDocument();
    expect(screen.getByText("sudo ffdb-host update-check")).toBeInTheDocument();
    expect(screen.getByText("sudo ffdb-host update")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open host update guide" })).toHaveAttribute("href", "/docs/host-updates");
  });

  it("keeps a persisted job visible while the API restarts, then reconnects and reports completion", async () => {
    let jobReads = 0;
    let statusReads = 0;
    const notice = vi.fn();
    const active = updateJob({ state: "running", phase: "restart", message: "Restarting FFDB services" });
    const client = await updateClient(async (request) => {
      const path = new URL(request.url).pathname;
      if (path === "/v1/instance/updates" && request.method === "GET") {
        statusReads += 1;
        if (statusReads === 2) throw new TypeError("connection closed during restart");
        const completed = statusReads >= 3;
        return Response.json({ ...updateStatus(completed), active_job: completed ? null : active });
      }
      if (path === "/v1/instance/updates/jobs/job-1") {
        jobReads += 1;
        return Response.json(updateJob({ state: "running", phase: "restart", message: "Restarting FFDB services" }));
      }
      return missingResponse();
    });

    render(<InstanceUpdatesPanel client={client} onNotice={notice} onReleaseChange={vi.fn()} />);

    expect(await screen.findByText("Restarting FFDB services")).toBeInTheDocument();
    expect(await screen.findByText("Reconnecting to FFDB…", {}, { timeout: 2_000 })).toBeInTheDocument();
    expect(screen.getByText(/gateway can remain available while the API restarts/i)).toBeInTheDocument();
    await waitFor(() => expect(notice).toHaveBeenCalledWith("Updated to FFDB 0.3.3"), { timeout: 8_000 });
    expect(jobReads).toBe(0);
    expect(statusReads).toBe(3);
  });

  it("reconciles an install when the restart replaces the submission response with a 503", async () => {
    let statusReads = 0;
    let installAttempts = 0;
    let jobReads = 0;
    const notice = vi.fn();
    const client = await updateClient(async (request) => {
      const path = new URL(request.url).pathname;
      if (path === "/v1/instance/updates" && request.method === "GET") {
        statusReads += 1;
        if (statusReads === 2) {
          return Response.json({ error: { code: "gateway.unavailable", message: "Service Unavailable", request_id: "restart-503" } }, { status: 503 });
        }
        return Response.json(updateStatus(statusReads >= 3));
      }
      if (path === "/v1/instance/updates/install") {
        installAttempts += 1;
        return Response.json({ error: { code: "gateway.unavailable", message: "Service Unavailable", request_id: "install-503" } }, { status: 503 });
      }
      if (path.includes("/v1/instance/updates/jobs/")) {
        jobReads += 1;
      }
      return missingResponse();
    });

    render(<InstanceUpdatesPanel client={client} onNotice={notice} onReleaseChange={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Review update" }));
    fireEvent.click(screen.getByRole("button", { name: "Install update" }));

    expect(await screen.findByText("Reconnecting to FFDB…", {}, { timeout: 2_000 })).toBeInTheDocument();
    expect(screen.queryByText("Host update request failed")).not.toBeInTheDocument();
    await waitFor(() => expect(notice).toHaveBeenCalledWith("Updated to FFDB 0.3.3"), { timeout: 8_000 });
    expect(screen.getByText("Signed release installed and readiness verified")).toBeInTheDocument();
    expect(installAttempts).toBe(1);
    expect(statusReads).toBe(3);
    expect(jobReads).toBe(0);
  });

  it("times out a poll stalled by the restart and reconciles completion from installed status", async () => {
    let statusReads = 0;
    const notice = vi.fn();
    const active = updateJob({ state: "running", phase: "restart", message: "Restarting FFDB services" });
    const client = await updateClient(async (request) => {
      const path = new URL(request.url).pathname;
      if (path === "/v1/instance/updates" && request.method === "GET") {
        statusReads += 1;
        if (statusReads === 2) {
          return new Promise<Response>((_resolve, reject) => {
            request.signal.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true });
          });
        }
        const completed = statusReads >= 3;
        return Response.json({ ...updateStatus(completed), active_job: completed ? null : active });
      }
      return missingResponse();
    });

    render(<InstanceUpdatesPanel client={client} onNotice={notice} onReleaseChange={vi.fn()} />);

    expect(await screen.findByText("Restarting FFDB services")).toBeInTheDocument();
    expect(await screen.findByText("Reconnecting to FFDB…", {}, { timeout: 5_000 })).toBeInTheDocument();
    await waitFor(() => expect(notice).toHaveBeenCalledWith("Updated to FFDB 0.3.3"), { timeout: 8_000 });
    expect(statusReads).toBe(3);
    expect(screen.getByText("Signed release installed and readiness verified")).toBeInTheDocument();
  }, 10_000);

  it("turns legacy nested updater JSON into one actionable failed-job card", async () => {
    const active = updateJob({ state: "running", phase: "backup", message: "Creating mandatory pre-update backup" });
    const failed = updateJob({
      state: "failed",
      phase: "failed",
      message: JSON.stringify({ code: "invalid_request", message: "verified release extraction failed", retryable: false }),
      error_code: "invalid_request",
      backup_path: "/var/lib/ffdb/backups/pre-update-0.3.2-to-0.3.3.tar.gz",
    });
    const client = await updateClient(async (request) => {
      const path = new URL(request.url).pathname;
      if (path === "/v1/instance/updates") return Response.json({ ...updateStatus(), active_job: active });
      if (path === "/v1/instance/updates/jobs/job-1") return Response.json(failed);
      return missingResponse();
    });

    render(<InstanceUpdatesPanel client={client} onNotice={() => undefined} />);

    expect(await screen.findByText("The updater sandbox blocked release extraction", {}, { timeout: 2_000 })).toBeInTheDocument();
    expect(screen.queryByText("Host update request failed")).not.toBeInTheDocument();
    expect(screen.queryByText(failed.message)).not.toBeInTheDocument();
    expect(screen.getByText(/kept the installed release active and preserved the pre-update backup/i)).toBeInTheDocument();
    fireEvent.click(screen.getByText("Technical details"));
    expect(screen.getByText("verified release extraction failed")).toBeInTheDocument();
    expect(screen.getByText("invalid_request")).toBeInTheDocument();
    expect(screen.getByText(failed.backup_path!)).toBeInTheDocument();
  });

  it("requires a UTC maintenance window before offering an automatic-install policy mutation", async () => {
    const calls: Request[] = [];
    const client = await updateClient(async (request) => {
      calls.push(request);
      if (request.url.endsWith("/v1/instance/updates")) return Response.json(updateStatus());
      return missingResponse();
    });

    render(<InstanceUpdatesPanel client={client} onNotice={() => undefined} />);
    await screen.findByRole("heading", { name: "Update policy" });
    fireEvent.click(screen.getByRole("checkbox", { name: /Install automatically/i }));
    expect(screen.getByLabelText("Maintenance window start UTC")).toHaveValue("03:00");
    fireEvent.change(screen.getByLabelText("Maintenance window start UTC"), { target: { value: "05:30" } });
    fireEvent.click(screen.getByRole("button", { name: "Save policy" }));
    const dialog = screen.getByRole("dialog", { name: "Apply this update policy?" });
    expect(within(dialog).getByText(/05:30 UTC for 60 minutes/i)).toBeInTheDocument();
    expect(calls.some((request) => request.method === "PATCH")).toBe(false);
  });
});

async function updateClient(fetcher: (request: Request) => Promise<Response> | Response): Promise<FFDBClient> {
  const store = new MemoryDeveloperSessionStore(`updates-${Math.random()}`);
  await store.set({ session_token: "owner-session", user_id: "owner-1", email: "owner@example.test", expires_at_ms: Date.now() + 60_000 });
  return new FFDBClient({
    baseUrl: "https://ffdb.example.test",
    developerSessionStore: store,
    fetch: async (input, init) => fetcher(new Request(input, init)),
  });
}

function updateStatus(installed = false): HostUpdateStatus {
  return {
    supported: true,
    unavailable_reason: null,
    capabilities: { check: true, install: true, rollback: true, automatic_checks: true, automatic_apply: true },
    state_schema: 1,
    minimum_rollback_version: "0.3.1",
    signature_identity: "https://github.com/Forever-Frameworks-LLC/ffdb/.github/workflows/release.yml@refs/tags/v0.3.3",
    installed_version: installed ? "0.3.3" : "0.3.2",
    available_version: installed ? "0.3.3" : "0.3.3",
    update_available: !installed,
    last_check_at_ms: 1_786_000_000_000,
    active_job: null,
    releases: [
      { version: installed ? "0.3.3" : "0.3.2", active: true, rollback_compatible: true, state_schema: 1, minimum_rollback_version: "0.3.1", signature_verified: true, signature_identity: "release-workflow", release_url: null },
      { version: "0.3.3", active: installed, rollback_compatible: true, state_schema: 1, minimum_rollback_version: "0.3.1", signature_verified: true, signature_identity: "https://github.com/Forever-Frameworks-LLC/ffdb/.github/workflows/release.yml@refs/tags/v0.3.3", release_url: "https://github.com/Forever-Frameworks-LLC/ffdb/releases/tag/v0.3.3" },
      { version: "0.3.1", active: false, rollback_compatible: true, state_schema: 1, minimum_rollback_version: null, signature_verified: true, signature_identity: "release-workflow", release_url: "https://github.com/Forever-Frameworks-LLC/ffdb/releases/tag/v0.3.1" },
    ],
    settings: { channel: "stable", automatic_checks: true, check_interval_hours: 24, automatic_apply: false, maintenance_window_start: null, maintenance_window_duration_minutes: 60 },
  };
}

function updateJob(overrides: Partial<HostUpdateJob> = {}): HostUpdateJob {
  return {
    job_id: "job-1",
    operation: "install",
    requested_version: "0.3.3",
    state: "running",
    phase: "restart",
    installed_version: "0.3.2",
    available_version: "0.3.3",
    previous_version: "0.3.2",
    backup_path: null,
    message: "Restarting FFDB services",
    error_code: null,
    retryable: false,
    created_at_ms: Date.now(),
    updated_at_ms: Date.now(),
    ...overrides,
  };
}

function missingResponse(): Response {
  return Response.json({ error: { code: "route.missing", message: "missing route", request_id: "updates-test" } }, { status: 404 });
}
