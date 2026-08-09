import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { FFDBClient } from "@ffdb/client";

import { CommercePanel } from "./Commerce.js";

describe("project commerce administration", () => {
  afterEach(() => { cleanup(); vi.restoreAllMocks(); });

  it("configures BYO Stripe without resurfacing secrets and creates a product", async () => {
    const calls: Request[] = [];
    let configured = false;
    const products: Record<string, unknown>[] = [];
    const client = commerceClient(async (request) => {
      calls.push(request);
      const url = new URL(request.url);
      if (url.pathname.endsWith("/commerce/account/byo") && request.method === "POST") { configured = true; return Response.json(account()); }
      if (url.pathname.endsWith("/commerce/account")) return configured ? Response.json(account()) : commerceNotConfigured();
      if (url.pathname.endsWith("/commerce/products") && request.method === "POST") { const input = await request.clone().json() as { name: string; description: string | null; tax_code: string | null }; const product = { id: "product-1", project_id: "project-1", ...input, metadata: {}, status: "draft", created_at_ms: 1, updated_at_ms: 1 }; products.push(product); return Response.json(product); }
      if (url.pathname.endsWith("/commerce/products")) return Response.json(products);
      if (commerceCollection(url.pathname)) return Response.json([]);
      return missing();
    });

    render(<CommercePanel client={client} onNotice={() => undefined} />);
    expect(await screen.findByRole("tab", { name: /Catalog & prices/ })).toHaveAttribute("aria-selected", "true");
    expect(screen.queryByRole("heading", { name: "Payment provider" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Provider" }));
    expect(await screen.findByRole("heading", { name: "Payment provider" })).toBeInTheDocument();
    const byoTab = screen.getByRole("tab", { name: "Bring your own keys" });
    const connectTab = screen.getByRole("tab", { name: "Stripe Connect" });
    fireEvent.keyDown(byoTab, { key: "ArrowRight" });
    expect(connectTab).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(connectTab, { key: "ArrowLeft" });
    expect(byoTab).toHaveAttribute("aria-selected", "true");
    fireEvent.change(screen.getByLabelText("Stripe secret key"), { target: { value: "sk_test_private" } });
    fireEvent.change(screen.getByLabelText("Stripe webhook secret"), { target: { value: "whsec_private" } });
    fireEvent.click(screen.getByRole("button", { name: "Configure project Stripe" }));

    await waitFor(() => expect(calls.some((request) => new URL(request.url).pathname.endsWith("/commerce/account/byo"))).toBe(true));
    const configuredRequest = calls.find((request) => new URL(request.url).pathname.endsWith("/commerce/account/byo"));
    await expect(configuredRequest?.clone().json()).resolves.toEqual({ secret_key: "sk_test_private", webhook_secret: "whsec_private" });
    expect(screen.getByLabelText("Stripe secret key")).toHaveValue("");
    expect(screen.getByLabelText("Stripe webhook secret")).toHaveValue("");
    expect(screen.queryByText("sk_test_private")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: /Catalog & prices/ }));
    fireEvent.change(screen.getByLabelText("Product name"), { target: { value: "Team membership" } });
    fireEvent.change(screen.getByLabelText("Description"), { target: { value: "Recurring access for a project team" } });
    fireEvent.click(screen.getByRole("button", { name: "Create product" }));
    expect((await screen.findAllByText("Team membership")).length).toBeGreaterThan(0);
    const productRequest = calls.find((request) => new URL(request.url).pathname.endsWith("/commerce/products") && request.method === "POST");
    await expect(productRequest?.clone().json()).resolves.toMatchObject({ name: "Team membership", description: "Recurring access for a project team" });
  });

  it("updates fulfillment, refunds a payment, cancels a subscription, and inspects entitlements", async () => {
    vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    const calls: Request[] = [];
    const client = commerceClient(async (request) => {
      calls.push(request);
      const url = new URL(request.url);
      if (url.pathname.endsWith("/commerce/account")) return Response.json(account());
      if (url.pathname.endsWith("/commerce/products")) return Response.json([product()]);
      if (url.pathname.endsWith("/commerce/prices")) return Response.json([price()]);
      if (url.pathname.endsWith("/commerce/orders/order-1/fulfillment")) return Response.json({ ...order(), fulfillment_status: "fulfilled" });
      if (url.pathname.endsWith("/commerce/orders")) return Response.json([order()]);
      if (url.pathname.endsWith("/commerce/refunds")) return Response.json({ id: "refund-1", payment_id: "payment-1", status: "succeeded", amount_minor: 2500, currency: "usd", reason: "requested_by_customer", created_at_ms: 1, updated_at_ms: 1 });
      if (url.pathname.endsWith("/commerce/payments")) return Response.json([payment()]);
      if (url.pathname.endsWith("/commerce/subscriptions/subscription-1/cancel")) return Response.json({ ...subscription(), cancel_at_period_end: true });
      if (url.pathname.endsWith("/commerce/subscriptions")) return Response.json([subscription()]);
      if (url.pathname.endsWith("/commerce/customer-portal")) return Response.json({ url: "https://billing.stripe.test/session-1" });
      if (url.pathname.endsWith("/commerce/entitlements")) return Response.json([{ subject: { kind: "team", id: "team-1" }, key: "pro_access", value: { type: "enabled", value: true }, subscription_id: "subscription-1", order_id: null, valid_from_ms: 1, valid_until_ms: null }]);
      return missing();
    });

    const { rerender } = render(<CommercePanel client={client} onNotice={() => undefined} view="orders" />);
    expect(await screen.findByRole("heading", { name: "Orders and fulfillment" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Payments and refunds" })).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Fulfillment for order-1"), { target: { value: "fulfilled" } });
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    await waitFor(() => expect(calls.some((request) => new URL(request.url).pathname.endsWith("/commerce/orders/order-1/fulfillment"))).toBe(true));
    const fulfillment = calls.find((request) => new URL(request.url).pathname.endsWith("/commerce/orders/order-1/fulfillment"));
    await expect(fulfillment?.clone().json()).resolves.toEqual({ status: "fulfilled", note: null });

    fireEvent.click(screen.getByRole("tab", { name: /Refunds/ }));
    expect(screen.queryByRole("heading", { name: "Orders and fulfillment" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Submit refund" }));
    await waitFor(() => expect(calls.some((request) => new URL(request.url).pathname.endsWith("/commerce/refunds"))).toBe(true));
    const refund = calls.find((request) => new URL(request.url).pathname.endsWith("/commerce/refunds"));
    await expect(refund?.clone().json()).resolves.toMatchObject({ payment_id: "payment-1", amount_minor: null, reason: "requested_by_customer" });

    rerender(<CommercePanel client={client} onNotice={() => undefined} view="subscriptions" />);
    expect(await screen.findByRole("heading", { name: "Subscriptions" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Cancel at period end…" }));
    await waitFor(() => expect(calls.some((request) => new URL(request.url).pathname.endsWith("/commerce/subscriptions/subscription-1/cancel"))).toBe(true));
    const cancellation = calls.find((request) => new URL(request.url).pathname.endsWith("/commerce/subscriptions/subscription-1/cancel"));
    await expect(cancellation?.clone().json()).resolves.toEqual({ at_period_end: true });

    rerender(<CommercePanel client={client} onNotice={() => undefined} view="products" />);
    fireEvent.click(await screen.findByRole("tab", { name: "Customer portal" }));
    fireEvent.change(screen.getByLabelText("Customer subject"), { target: { value: "team" } });
    fireEvent.change(screen.getByLabelText("Customer subject ID"), { target: { value: "team-1" } });
    fireEvent.click(screen.getByRole("button", { name: "Create customer portal session" }));
    expect(await screen.findByRole("link", { name: /Open customer billing portal/ })).toHaveAttribute("href", "https://billing.stripe.test/session-1");
    const customerPortal = calls.find((request) => new URL(request.url).pathname.endsWith("/commerce/customer-portal"));
    await expect(customerPortal?.clone().json()).resolves.toMatchObject({ subject: { kind: "team", id: "team-1" } });
    const customerPortalBody = await customerPortal?.clone().json() as { return_url?: string };
    expect(new URL(customerPortalBody.return_url ?? "http://invalid").searchParams.get("commerce-customer-portal")).toBe("return");

    rerender(<CommercePanel client={client} onNotice={() => undefined} view="subscriptions" />);
    expect(await screen.findByRole("heading", { name: "Subscriptions" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Entitlement inspector" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("tab", { name: "Entitlements" }));
    fireEvent.change(screen.getByLabelText("Subject kind"), { target: { value: "team" } });
    fireEvent.change(screen.getAllByLabelText("Subject ID").at(-1)!, { target: { value: "team-1" } });
    fireEvent.click(screen.getByRole("button", { name: "Inspect entitlements" }));
    expect(await screen.findByText("pro_access")).toBeInTheDocument();
    expect(screen.getByText("Enabled")).toBeInTheDocument();
  });

  it("confirms commerce disconnect, preserves a provider-bound account on conflict, and clears it on success", async () => {
    vi.spyOn(globalThis, "confirm").mockReturnValue(true);
    const calls: Request[] = [];
    let configured = true;
    let disconnectAttempts = 0;
    const client = commerceClient(async (request) => {
      calls.push(request);
      const url = new URL(request.url);
      if (url.pathname.endsWith("/commerce/account") && request.method === "DELETE") {
        disconnectAttempts += 1;
        if (disconnectAttempts === 1) return Response.json({ error: { code: "commerce.account_in_use", message: "provider-bound state remains", request_id: "commerce-disconnect" } }, { status: 409 });
        configured = false;
        return new Response(null, { status: 204 });
      }
      if (url.pathname.endsWith("/commerce/account")) return configured ? Response.json(account()) : commerceNotConfigured();
      if (url.pathname.endsWith("/commerce/products") || commerceCollection(url.pathname)) return Response.json([]);
      return missing();
    });

    render(<CommercePanel client={client} onNotice={() => undefined} />);
    fireEvent.click(await screen.findByRole("tab", { name: "Provider" }));
    const disconnectButton = await screen.findByRole("button", { name: "Disconnect commerce…" });
    fireEvent.click(disconnectButton);
    expect(await screen.findByRole("alert")).toHaveTextContent(/provider-bound catalog, customer, order, or subscription records/i);
    expect(screen.getByRole("button", { name: "Disconnect commerce…" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Disconnect commerce…" }));
    expect(await screen.findByText("Not configured")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Disconnect commerce…" })).not.toBeInTheDocument();
    const deletes = calls.filter((request) => new URL(request.url).pathname.endsWith("/commerce/account") && request.method === "DELETE");
    expect(deletes).toHaveLength(2);
    expect(deletes.every((request) => request.headers.get("idempotency-key")?.startsWith("commerce-disconnect:") === true)).toBe(true);
  });

  it("keeps task panels exclusive and supports keyboard tab navigation", async () => {
    const client = commerceClient(async (request) => {
      const pathname = new URL(request.url).pathname;
      if (pathname.endsWith("/commerce/account")) return Response.json(account());
      if (pathname.endsWith("/commerce/products")) return Response.json([product()]);
      if (pathname.endsWith("/commerce/prices")) return Response.json([price()]);
      if (commerceCollection(pathname)) return Response.json([]);
      return missing();
    });

    render(<CommercePanel client={client} onNotice={() => undefined} />);
    const catalog = await screen.findByRole("tab", { name: /Catalog & prices/ });
    expect(catalog).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("heading", { name: "Products" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Checkout test" })).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Payment provider" })).not.toBeInTheDocument();

    fireEvent.keyDown(catalog, { key: "ArrowRight" });
    const checkout = screen.getByRole("tab", { name: "Checkout" });
    expect(checkout).toHaveAttribute("aria-selected", "true");
    expect(checkout).toHaveFocus();
    expect(screen.getByRole("heading", { name: "Checkout test" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Products" })).not.toBeInTheDocument();

    fireEvent.keyDown(checkout, { key: "End" });
    expect(screen.getByRole("tab", { name: "Provider" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("heading", { name: "Payment provider" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Checkout test" })).not.toBeInTheDocument();
  });
});

function commerceClient(handler: (request: Request) => Promise<Response>): FFDBClient {
  return new FFDBClient({ baseUrl: "https://ffdb.example.test", projectId: "project-1", developerKey: "ffdb_dev_commerce.secret", fetch: async (input, init) => handler(new Request(input, init)) });
}
function commerceCollection(pathname: string): boolean { return ["/commerce/prices", "/commerce/orders", "/commerce/payments", "/commerce/subscriptions"].some((suffix) => pathname.endsWith(suffix)); }
function commerceNotConfigured(): Response { return Response.json({ error: { code: "commerce.account_not_configured", message: "not configured", request_id: "commerce-test" } }, { status: 409 }); }
function missing(): Response { return Response.json({ error: { code: "route.missing", message: "missing", request_id: "commerce-test" } }, { status: 404 }); }
function account() { return { project_id: "project-1", mode: "bring_your_own_keys", status: "enabled", livemode: false, provider_account_id: null, capabilities: { one_time_payments: true, recurring_payments: true, refunds: true, customer_portal: true }, requirements_due: [], disabled_reason: null, webhook_url: "https://ffdb.example.test/v1/projects/project-1/commerce/webhooks/stripe", secrets_configured: true }; }
function product() { return { id: "product-1", project_id: "project-1", name: "Pro membership", description: "Full access", tax_code: null, status: "active", metadata: {}, created_at_ms: 1, updated_at_ms: 1 }; }
function price() { return { id: "price-1", project_id: "project-1", product_id: "product-1", lookup_key: "pro_monthly", currency: "usd", unit_amount_minor: 2500, billing: { type: "recurring", interval: "month", interval_count: 1 }, entitlements: { pro_access: { type: "enabled", value: true } }, active: true, created_at_ms: 1 }; }
function order() { return { id: "order-1", project_id: "project-1", customer_id: "customer-1", client_reference: null, status: "paid", fulfillment_status: "unfulfilled", currency: "usd", subtotal_minor: 2500, discount_minor: 0, tax_minor: 0, shipping_minor: 0, total_minor: 2500, refunded_minor: 0, lines: [{ product_id: "product-1", price_id: "price-1", product_name: "Pro membership", currency: "usd", unit_amount_minor: 2500, quantity: 1, line_total_minor: 2500 }], paid_at_ms: 1, created_at_ms: 1, updated_at_ms: 1 }; }
function payment() { return { id: "payment-1", project_id: "project-1", order_id: "order-1", subscription_id: "subscription-1", status: "captured", currency: "usd", authorized_minor: 2500, captured_minor: 2500, refunded_minor: 0, provider_created_at_ms: 1, created_at_ms: 1 }; }
function subscription() { return { id: "subscription-1", project_id: "project-1", customer_id: "customer-1", price_id: "price-1", subject: { kind: "team", id: "team-1" }, quantity: 1, status: "active", current_period_start_ms: 1, current_period_end_ms: 10_000, cancel_at_period_end: false, created_at_ms: 1, updated_at_ms: 1 }; }
