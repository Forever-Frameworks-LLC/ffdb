import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { ManagedTable } from "./ManagedTable.js";

afterEach(cleanup);

describe("ManagedTable growing-data contract", () => {
  const rows = Array.from({ length: 12 }, (_, index) => [
    `Member ${String(index + 1).padStart(2, "0")}`,
    index === 11 ? "target@example.test" : `member-${index + 1}@example.test`,
  ] as const);

  it("searches, sorts, and paginates without losing the result count", () => {
    render(<ManagedTable headings={["Name", "Email"]} label="members" pageSizes={[10, 25]} rows={rows} />);

    expect(screen.getByText("Showing 1–10 of 12")).toBeInTheDocument();
    expect(screen.getByText("Member 01")).toBeInTheDocument();
    expect(screen.queryByText("Member 12")).not.toBeInTheDocument();

    const nameSort = screen.getByRole("button", { name: "Name" });
    fireEvent.click(nameSort);
    expect(nameSort.closest("th")).toHaveAttribute("aria-sort", "ascending");
    fireEvent.click(nameSort);
    expect(nameSort.closest("th")).toHaveAttribute("aria-sort", "descending");
    expect(within(screen.getAllByRole("rowgroup")[1]!).getAllByRole("row")[0]).toHaveTextContent("Member 12");

    fireEvent.change(screen.getByPlaceholderText("Search members"), { target: { value: "target" } });
    expect(screen.getByText("target@example.test")).toBeInTheDocument();
    expect(screen.getByText("Showing 1–1 of 1")).toBeInTheDocument();
    expect(screen.getByText("1 member")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Clear" }));
    fireEvent.click(screen.getByRole("button", { name: "Next page of members" }));
    expect(screen.getByText("Showing 11–12 of 12")).toBeInTheDocument();
    expect(screen.getByText("Member 01")).toBeInTheDocument();
  });

  it("keeps search, sorting, and paging controls out of a true empty state", () => {
    render(<ManagedTable headings={["Name"]} label="members" rows={[]} empty="No members yet." />);
    expect(screen.getByText("No members yet.")).toBeInTheDocument();
    expect(screen.queryByPlaceholderText("Search members")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /page of members/i })).not.toBeInTheDocument();
  });

  it("can defer pagination to a server-backed parent without hiding loaded rows", () => {
    render(<ManagedTable headings={["Name", "Email"]} label="members" pagination={false} rows={rows} />);

    expect(screen.getByRole("table", { name: "members" })).toBeInTheDocument();
    expect(screen.getByText("Member 12")).toBeInTheDocument();
    expect(screen.queryByText(/Showing /)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /page of members/i })).not.toBeInTheDocument();
  });
});
