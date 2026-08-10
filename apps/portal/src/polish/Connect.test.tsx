import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { PortalConfiguration } from "../config.js";
import { ConnectPanel } from "./Connect.js";

const configuration: PortalConfiguration = {
  apiUrl: "https://ffdb.example.test/",
  organizationId: "org-1",
  projectId: "project-atlas",
  developerKey: "ffdb_dev_portal.must-not-render",
  projectName: "Atlas",
  organizationName: "Northstar Labs",
};

describe("ConnectPanel", () => {
  const writeText = vi.fn().mockResolvedValue(undefined);

  beforeEach(() => {
    writeText.mockClear();
    Object.defineProperty(globalThis.navigator, "clipboard", { configurable: true, value: { writeText } });
  });

  afterEach(() => cleanup());

  it("renders selected-project React configuration without exposing the portal credential", () => {
    const { container } = render(<ConnectPanel configuration={configuration} onNotice={vi.fn()} onOpenAuth={vi.fn()} />);

    expect(screen.getByRole("heading", { name: "Bring Atlas into your app." })).toBeInTheDocument();
    expect(screen.getAllByText("project-atlas").length).toBeGreaterThan(0);
    expect(screen.getAllByText("https://ffdb.example.test").length).toBeGreaterThan(0);
    expect(screen.getByText(/@ffdb\/client@\d+\.\d+\.\d+/)).toBeInTheDocument();
    expect(container).not.toHaveTextContent("must-not-render");
    expect(container).not.toHaveTextContent("127.0.0.1:5180");
    expect(screen.queryByText("Localhost checklist")).not.toBeInTheDocument();
  });

  it("switches to Expo and trusted Node guidance with keyboard-accessible tabs", () => {
    render(<ConnectPanel configuration={configuration} onNotice={vi.fn()} onOpenAuth={vi.fn()} />);

    const reactTab = screen.getByRole("tab", { name: "React web" });
    fireEvent.keyDown(reactTab, { key: "ArrowRight" });
    expect(screen.getByRole("tab", { name: "Expo / native" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText(/EXPO_PUBLIC_FFDB_PROJECT_ID=project-atlas/)).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Before your first Expo / native request" })).toBeInTheDocument();
    expect(screen.getByText(/iOS and Android API calls do not use browser CORS/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Configure auth and web URLs/i })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: /Node/ }));
    expect(screen.getByText(/FFDB_DEVELOPER_KEY=ffdb_dev_replace_me/)).toBeInTheDocument();
    expect(screen.getByText("Server-side only")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Before your first Node request" })).toBeInTheDocument();
    expect(screen.getByText("No browser URL allowlist required")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Configure .* URLs/i })).not.toBeInTheDocument();
  });

  it("copies generated values and routes key management to Settings", async () => {
    const onNotice = vi.fn();
    const onOpenAuth = vi.fn();
    render(<ConnectPanel configuration={configuration} onNotice={onNotice} onOpenAuth={onOpenAuth} />);

    fireEvent.click(screen.getByRole("button", { name: "Copy Project ID" }));
    expect(writeText).toHaveBeenCalledWith("project-atlas");
    expect(await screen.findByRole("button", { name: "Project ID copied" })).toBeInTheDocument();
    expect(onNotice).toHaveBeenCalledWith("Project ID copied to the clipboard.");

    const readiness = screen.getByRole("heading", { name: "Before your first React web request" }).closest("section")!;
    fireEvent.click(within(readiness).getByRole("button", { name: /Configure web and auth URLs/i }));
    expect(onOpenAuth).toHaveBeenCalledTimes(1);
  });
});
