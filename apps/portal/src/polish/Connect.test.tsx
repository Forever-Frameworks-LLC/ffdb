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
    expect(screen.getByText(/@ffdb\/client@0\.3\.5/)).toBeInTheDocument();
    expect(container).not.toHaveTextContent("must-not-render");
  });

  it("switches to Expo and trusted Node guidance with keyboard-accessible tabs", () => {
    render(<ConnectPanel configuration={configuration} onNotice={vi.fn()} onOpenAuth={vi.fn()} />);

    const reactTab = screen.getByRole("tab", { name: "React web" });
    fireEvent.keyDown(reactTab, { key: "ArrowRight" });
    expect(screen.getByRole("tab", { name: "Expo / native" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText(/EXPO_PUBLIC_FFDB_PROJECT_ID=project-atlas/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: /Node/ }));
    expect(screen.getByText(/FFDB_DEVELOPER_KEY=ffdb_dev_replace_me/)).toBeInTheDocument();
    expect(screen.getByText("Server-side only")).toBeInTheDocument();
  });

  it("copies generated values and routes key management to Settings", async () => {
    const onNotice = vi.fn();
    const onOpenAuth = vi.fn();
    render(<ConnectPanel configuration={configuration} onNotice={onNotice} onOpenAuth={onOpenAuth} />);

    fireEvent.click(screen.getByRole("button", { name: "Copy Project ID" }));
    expect(writeText).toHaveBeenCalledWith("project-atlas");
    expect(await screen.findByRole("button", { name: "Project ID copied" })).toBeInTheDocument();
    expect(onNotice).toHaveBeenCalledWith("Project ID copied to the clipboard.");

    const readiness = screen.getByRole("heading", { name: "From empty folder to first request" }).closest("section")!;
    fireEvent.click(within(readiness).getByRole("button", { name: /Open application URLs/i }));
    expect(onOpenAuth).toHaveBeenCalledTimes(1);
  });
});
