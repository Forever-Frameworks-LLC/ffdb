import { useCallback, useEffect, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";

import {
  FFDBClient,
  FFDBError,
  type CommerceAccountSummary,
  type CommerceEntitlementSummary,
  type CommerceEntitlementValue,
  type CommerceFulfillmentStatus,
  type CommerceOrderSummary,
  type CommercePaymentSummary,
  type CommercePriceBilling,
  type CommercePriceSummary,
  type CommerceProductSummary,
  type CommerceSubscriptionSummary,
} from "@ffdb/client";

import { Icon } from "./icons.js";
import { ManagedTable } from "./polish/ManagedTable.js";
import "./commerce.css";

interface CommerceData {
  readonly account: CommerceAccountSummary | null;
  readonly products: readonly CommerceProductSummary[];
  readonly prices: readonly CommercePriceSummary[];
  readonly orders: readonly CommerceOrderSummary[];
  readonly payments: readonly CommercePaymentSummary[];
  readonly subscriptions: readonly CommerceSubscriptionSummary[];
}

type CommerceView = "products" | "orders" | "subscriptions";
type CommerceTask = "catalog" | "checkout" | "customer-portal" | "provider" | "orders" | "payments" | "subscriptions" | "entitlements";

interface CommerceTaskDefinition {
  readonly id: CommerceTask;
  readonly label: string;
  readonly count?: number;
}

export function CommercePanel({ client, onNotice, view = "products" }: { readonly client: FFDBClient; readonly view?: CommerceView; onNotice(value: string): void }) {
  const [revision, setRevision] = useState(0);
  const [data, setData] = useState<CommerceData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [onboardingUrl, setOnboardingUrl] = useState<string | null>(null);
  const [checkoutUrl, setCheckoutUrl] = useState<string | null>(null);
  const [selectedTask, setSelectedTask] = useState<CommerceTask>(() => initialTaskForView(view));

  const load = useCallback(async () => {
    setError(null);
    try {
      const accountPromise = client.commerce.account().catch((cause: unknown) => {
        if (cause instanceof FFDBError && cause.code === "commerce.account_not_configured") return null;
        throw cause;
      });
      const [account, products, prices, orders, payments, subscriptions] = await Promise.all([
        accountPromise,
        client.commerce.products(true),
        client.commerce.prices(true),
        client.commerce.orders(),
        client.commerce.payments(),
        client.commerce.subscriptions(),
      ]);
      setData({ account, products, prices, orders, payments, subscriptions });
    } catch (cause) {
      setError(errorMessage(cause));
    }
  }, [client, revision]);

  useEffect(() => { void load(); }, [load]);
  useEffect(() => { setSelectedTask(initialTaskForView(view)); }, [view]);

  const refresh = (message?: string) => {
    if (message !== undefined) onNotice(message);
    setRevision((value) => value + 1);
  };

  if (data === null && error === null) return <section className="commerce-route-state" aria-busy="true" aria-live="polite"><span className="commerce-route-state__spinner" /><div><h2>Loading project commerce</h2><p>Fetching provider status, catalog, orders, payments, and subscriptions.</p></div></section>;
  if (data === null) return <section className="commerce-route-state"><CommerceError message={error ?? "Project commerce is unavailable."} retry={() => void load()} /></section>;

  const tasks = tasksForView(view, data);
  const activeTask = tasks.some((task) => task.id === selectedTask) ? selectedTask : initialTaskForView(view);

  return <div className="commerce-admin commerce-workspace">
    {error === null ? null : <CommerceError message={error} retry={() => void load()} />}
    <CommerceWorkspaceBar account={data.account} activeTask={activeTask} tasks={tasks} view={view} onTask={setSelectedTask} />
    <section className="commerce-task-panel" id={`commerce-panel-${view}-${activeTask}`} role="tabpanel" aria-labelledby={`commerce-tab-${view}-${activeTask}`} tabIndex={0}>
      {view === "products" && activeTask === "catalog" ? <div className="management-grid commerce-management-grid commerce-catalog-grid"><ProductCatalogPanel client={client} products={data.products} onChanged={refresh} /><PriceCatalogPanel client={client} prices={data.prices} products={data.products} onChanged={refresh} /></div> : null}
      {view === "products" && activeTask === "checkout" ? <CheckoutPanel client={client} prices={data.prices} products={data.products} checkoutUrl={checkoutUrl} onCheckout={setCheckoutUrl} /> : null}
      {view === "products" && activeTask === "customer-portal" ? <CustomerPortalPanel account={data.account} client={client} /> : null}
      {view === "products" && activeTask === "provider" ? <CommerceAccountPanel account={data.account} client={client} onboardingUrl={onboardingUrl} onOnboarding={setOnboardingUrl} onChanged={refresh} /> : null}
      {view === "orders" && activeTask === "orders" ? <OrdersPanel client={client} orders={data.orders} onChanged={refresh} /> : null}
      {view === "orders" && activeTask === "payments" ? <PaymentsPanel client={client} payments={data.payments} onChanged={refresh} /> : null}
      {view === "subscriptions" && activeTask === "subscriptions" ? <SubscriptionsPanel client={client} subscriptions={data.subscriptions} onChanged={refresh} /> : null}
      {view === "subscriptions" && activeTask === "entitlements" ? <EntitlementsPanel client={client} /> : null}
    </section>
  </div>;
}

function CommerceWorkspaceBar({ account, activeTask, tasks, view, onTask }: { readonly account: CommerceAccountSummary | null; readonly activeTask: CommerceTask; readonly tasks: readonly CommerceTaskDefinition[]; readonly view: CommerceView; onTask(task: CommerceTask): void }) {
  const viewTitle = commerceViewTitle(view);
  const providerLabel = account === null ? "Provider not configured" : `${account.mode === "stripe_connect" ? "Stripe Connect" : "Bring your own Stripe"} · ${humanize(account.status)}`;
  const providerDetail = account === null ? "Set up Stripe before accepting payments" : account.disabled_reason ?? (account.livemode ? "Live mode" : "Test mode");

  return <header className="commerce-workspace-bar">
    <div className={account?.status === "enabled" ? "commerce-provider-chip is-enabled" : "commerce-provider-chip"} aria-label={`${providerLabel}. ${providerDetail}`}>
      <span className="commerce-provider-dot" aria-hidden="true" />
      <span><strong>{providerLabel}</strong><small>{providerDetail}</small></span>
    </div>
    <div className="commerce-task-tabs" role="tablist" aria-label={`${viewTitle} tasks`}>
      {tasks.map((task, index) => <button
        aria-controls={`commerce-panel-${view}-${task.id}`}
        aria-selected={activeTask === task.id}
        id={`commerce-tab-${view}-${task.id}`}
        key={task.id}
        role="tab"
        tabIndex={activeTask === task.id ? 0 : -1}
        type="button"
        onClick={() => onTask(task.id)}
        onKeyDown={(event) => handleTaskKeyDown(event, tasks, index, onTask)}
      >
        {task.label}{task.count === undefined ? null : <span>{task.count}</span>}
      </button>)}
    </div>
  </header>;
}

function handleTaskKeyDown(event: KeyboardEvent<HTMLButtonElement>, tasks: readonly CommerceTaskDefinition[], index: number, onTask: (task: CommerceTask) => void) {
  let nextIndex: number | null = null;
  if (event.key === "ArrowRight") nextIndex = (index + 1) % tasks.length;
  if (event.key === "ArrowLeft") nextIndex = (index - 1 + tasks.length) % tasks.length;
  if (event.key === "Home") nextIndex = 0;
  if (event.key === "End") nextIndex = tasks.length - 1;
  if (nextIndex === null) return;
  event.preventDefault();
  const nextTask = tasks[nextIndex];
  if (nextTask === undefined) return;
  onTask(nextTask.id);
  event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role='tab']").item(nextIndex).focus();
}

function CommerceAccountPanel({ account, client, onboardingUrl, onOnboarding, onChanged }: { readonly account: CommerceAccountSummary | null; readonly client: FFDBClient; readonly onboardingUrl: string | null; onOnboarding(url: string | null): void; onChanged(message?: string): void }) {
  const [mode, setMode] = useState<"byo" | "connect">(account?.mode === "stripe_connect" ? "connect" : "byo");
  const [secretKey, setSecretKey] = useState("");
  const [webhookSecret, setWebhookSecret] = useState("");
  const [country, setCountry] = useState("US");
  const [email, setEmail] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const configureByo = async (event: FormEvent) => {
    event.preventDefault(); setPending(true); setError(null); onOnboarding(null);
    try { await client.commerce.configureByo({ secret_key: secretKey, webhook_secret: webhookSecret }); setSecretKey(""); setWebhookSecret(""); onChanged("Project Stripe credentials updated"); }
    catch (cause) { setError(errorMessage(cause)); }
    finally { setPending(false); }
  };
  const connect = async (event: FormEvent) => {
    event.preventDefault(); setPending(true); setError(null);
    try { const result = await client.commerce.connectOnboarding({ country, email, return_url: commerceReturnUrl("return"), refresh_url: commerceReturnUrl("refresh") }); onOnboarding(result.onboarding_url); onChanged("Stripe Connect onboarding link created"); }
    catch (cause) { setError(errorMessage(cause)); }
    finally { setPending(false); }
  };
  const refresh = async () => { setPending(true); setError(null); try { await client.commerce.refreshAccount(); onChanged("Project commerce account refreshed"); } catch (cause) { setError(errorMessage(cause)); } finally { setPending(false); } };
  const disconnect = async () => {
    if (!globalThis.confirm("Disconnect this project's commerce account? Checkout, provider refresh, refunds, and subscription operations will stop until another account is configured.")) return;
    setPending(true);
    setError(null);
    try {
      await client.commerce.disconnectAccount({ idempotencyKey: `commerce-disconnect:${globalThis.crypto.randomUUID()}` });
      onOnboarding(null);
      onChanged("Project commerce account disconnected");
    } catch (cause) {
      setError(cause instanceof FFDBError && cause.code === "commerce.account_in_use"
        ? "Disconnect blocked: this account still has provider-bound catalog, customer, order, or subscription records. Keep the provider configured until those records are migrated or retired through a supported lifecycle."
        : errorMessage(cause));
    } finally {
      setPending(false);
    }
  };

  return <section className="management-panel commerce-account-panel">
    <div className="management-header"><div><h2>Payment provider</h2><p>Use the project's own Stripe credentials or onboard a connected account. Neither option affects FFDB platform billing.</p></div>{account === null ? null : <div className="action-row"><button disabled={pending} type="button" onClick={() => void refresh()}>Refresh provider</button><button className="danger-action" disabled={pending} type="button" onClick={() => void disconnect()}>Disconnect commerce…</button></div>}</div>
    <div className="commerce-account-status">
      <span className={account?.status === "enabled" ? "status-dot" : "status-dot attention"} />
      <div><strong>{account === null ? "Not configured" : `${account.mode === "stripe_connect" ? "Stripe Connect" : "Bring your own Stripe"} · ${humanize(account.status)}`}</strong><p>{account === null ? "Choose a provider mode below to begin accepting payments." : account.disabled_reason ?? (account.livemode ? "Live mode" : "Test mode")}</p></div>
      {account === null ? null : <div className="commerce-capabilities" aria-label="Commerce capabilities"><Capability enabled={account.capabilities.one_time_payments} label="One-time" /><Capability enabled={account.capabilities.recurring_payments} label="Recurring" /><Capability enabled={account.capabilities.refunds} label="Refunds" /><Capability enabled={account.capabilities.customer_portal} label="Customer portal" /></div>}
    </div>
    <div className="provider-mode-tabs" role="tablist" aria-label="Project payment provider"><button id="provider-tab-byo" aria-controls="provider-panel-byo" aria-selected={mode === "byo"} className={mode === "byo" ? "selected" : ""} onClick={() => setMode("byo")} onKeyDown={(event) => handleProviderTabKeyDown(event, "byo", setMode)} role="tab" tabIndex={mode === "byo" ? 0 : -1} type="button">Bring your own keys</button><button id="provider-tab-connect" aria-controls="provider-panel-connect" aria-selected={mode === "connect"} className={mode === "connect" ? "selected" : ""} onClick={() => setMode("connect")} onKeyDown={(event) => handleProviderTabKeyDown(event, "connect", setMode)} role="tab" tabIndex={mode === "connect" ? 0 : -1} type="button">Stripe Connect</button></div>
    {mode === "byo" ? <form aria-labelledby="provider-tab-byo" className="commerce-provider-form" id="provider-panel-byo" role="tabpanel" onSubmit={(event) => void configureByo(event)}><Field label="Stripe secret key" type="password" value={secretKey} onChange={setSecretKey} /><Field label="Stripe webhook secret" type="password" value={webhookSecret} onChange={setWebhookSecret} /><button className="primary-action" disabled={pending} type="submit">{pending ? "Saving…" : account?.mode === "bring_your_own_keys" ? "Rotate encrypted credentials" : "Configure project Stripe"}</button><p>Secrets are encrypted by the API and never returned to this portal.</p></form> : <form aria-labelledby="provider-tab-connect" className="commerce-provider-form" id="provider-panel-connect" role="tabpanel" onSubmit={(event) => void connect(event)}><Field label="Connected account country" value={country} onChange={(value) => setCountry(value.toUpperCase())} /><Field label="Connected account email" type="email" value={email} onChange={setEmail} /><button className="primary-action" disabled={pending} type="submit">{pending ? "Preparing…" : "Create onboarding link"}</button><p>Your project is merchant of record and receives direct charges on its connected account.</p></form>}
    {onboardingUrl === null ? null : <a className="billing-redirect" href={onboardingUrl}>Continue to Stripe <Icon name="external" size={15} /></a>}
    {account?.requirements_due.length ? <p className="security-note">Stripe still requires: {account.requirements_due.join(", ")}.</p> : null}
    {error === null ? null : <div className="access-error" role="alert">{error}</div>}
  </section>;
}

function ProductCatalogPanel({ products, client, onChanged }: { readonly products: readonly CommerceProductSummary[]; readonly client: FFDBClient; onChanged(message?: string): void }) {
  const [name, setName] = useState(""); const [description, setDescription] = useState(""); const [taxCode, setTaxCode] = useState(""); const [error, setError] = useState<string | null>(null);
  const create = async (event: FormEvent) => { event.preventDefault(); setError(null); try { await client.commerce.createProduct({ name, description: description.trim() || null, tax_code: taxCode.trim() || null }); setName(""); setDescription(""); setTaxCode(""); onChanged("Product created"); } catch (cause) { setError(errorMessage(cause)); } };
  const archive = async (product: CommerceProductSummary) => { if (!globalThis.confirm(`Archive ${product.name}? New checkouts will stop using it.`)) return; try { await client.commerce.archiveProduct(product.id); onChanged("Product archived"); } catch (cause) { setError(errorMessage(cause)); } };
  return <section className="management-panel span-two"><PanelHeading title="Products" detail="Define what your application sells. Provider product records are created from these durable catalog entries." /><form className="commerce-inline-form" onSubmit={(event) => void create(event)}><Field label="Product name" value={name} onChange={setName} /><Field label="Description" value={description} onChange={setDescription} optional /><Field label="Stripe tax code" value={taxCode} onChange={setTaxCode} optional /><button className="primary-action" type="submit">Create product</button></form>{error === null ? null : <div className="access-error" role="alert">{error}</div>}<DataTable headings={["Product", "Status", "Tax code", "Updated", "Action"]} rows={products.map((product) => [<Entity key={product.id} title={product.name} detail={product.description ?? product.id} />, humanize(product.status), product.tax_code ?? "—", formatDate(product.updated_at_ms), <button disabled={product.status === "archived"} key={product.id} onClick={() => void archive(product)} type="button">{product.status === "archived" ? "Archived" : "Archive…"}</button>])} empty="No products yet." /></section>;
}

function PriceCatalogPanel({ products, prices, client, onChanged }: { readonly products: readonly CommerceProductSummary[]; readonly prices: readonly CommercePriceSummary[]; readonly client: FFDBClient; onChanged(message?: string): void }) {
  const activeProducts = products.filter((product) => product.status !== "archived");
  const [productId, setProductId] = useState(activeProducts[0]?.id ?? ""); const [amount, setAmount] = useState("1000"); const [currency, setCurrency] = useState("usd"); const [lookupKey, setLookupKey] = useState(""); const [kind, setKind] = useState<"one_time" | "recurring">("one_time"); const [interval, setInterval] = useState<"day" | "week" | "month" | "year">("month"); const [entitlementKey, setEntitlementKey] = useState(""); const [entitlementValue, setEntitlementValue] = useState("true"); const [error, setError] = useState<string | null>(null);
  useEffect(() => { if (productId === "" && activeProducts[0] !== undefined) setProductId(activeProducts[0].id); }, [activeProducts, productId]);
  const create = async (event: FormEvent) => { event.preventDefault(); const billing: CommercePriceBilling = kind === "one_time" ? { type: "one_time" } : { type: "recurring", interval, interval_count: 1 }; const entitlements = entitlementKey.trim() === "" ? {} : { [entitlementKey.trim()]: entitlementFromInput(entitlementValue) }; setError(null); try { await client.commerce.createPrice({ product_id: productId, lookup_key: lookupKey.trim() || null, currency, unit_amount_minor: Number(amount), billing, entitlements }); setLookupKey(""); setEntitlementKey(""); onChanged("Price created"); } catch (cause) { setError(errorMessage(cause)); } };
  const retire = async (price: CommercePriceSummary) => { if (!globalThis.confirm("Retire this price? Existing subscriptions keep their current price snapshot.")) return; try { await client.commerce.retirePrice(price.id); onChanged("Price retired"); } catch (cause) { setError(errorMessage(cause)); } };
  return <section className="management-panel span-two"><PanelHeading title="Prices and entitlements" detail="Prices are immutable billing snapshots. Add an entitlement key to grant access after verified payment." /><form className="price-editor" onSubmit={(event) => void create(event)}><label className="field"><span>Product</span><select required value={productId} onChange={(event) => setProductId(event.target.value)}><option value="">Choose product</option>{activeProducts.map((product) => <option key={product.id} value={product.id}>{product.name}</option>)}</select></label><Field label="Amount (minor units)" type="number" value={amount} onChange={setAmount} /><Field label="Currency" value={currency} onChange={(value) => setCurrency(value.toLowerCase())} /><Field label="Lookup key" value={lookupKey} onChange={setLookupKey} optional /><label className="field"><span>Billing</span><select value={kind} onChange={(event) => setKind(event.target.value as typeof kind)}><option value="one_time">One time</option><option value="recurring">Recurring</option></select></label>{kind === "recurring" ? <label className="field"><span>Interval</span><select value={interval} onChange={(event) => setInterval(event.target.value as typeof interval)}><option value="day">Day</option><option value="week">Week</option><option value="month">Month</option><option value="year">Year</option></select></label> : null}<Field label="Entitlement key" value={entitlementKey} onChange={setEntitlementKey} optional /><Field label="Entitlement value" value={entitlementValue} onChange={setEntitlementValue} optional /><button className="primary-action" disabled={productId === ""} type="submit">Create immutable price</button></form>{error === null ? null : <div className="access-error" role="alert">{error}</div>}<DataTable headings={["Product", "Amount", "Billing", "Entitlements", "State", "Action"]} rows={prices.map((price) => [products.find((product) => product.id === price.product_id)?.name ?? price.product_id, money(price.unit_amount_minor, price.currency), billingLabel(price.billing), Object.keys(price.entitlements).join(", ") || "—", price.active ? "Active" : "Retired", <button disabled={!price.active} key={price.id} onClick={() => void retire(price)} type="button">{price.active ? "Retire…" : "Retired"}</button>])} empty="No prices yet." /></section>;
}

function CheckoutPanel({ products, prices, client, checkoutUrl, onCheckout }: { readonly products: readonly CommerceProductSummary[]; readonly prices: readonly CommercePriceSummary[]; readonly client: FFDBClient; readonly checkoutUrl: string | null; onCheckout(value: string | null): void }) {
  const active = prices.filter((price) => price.active);
  const [priceId, setPriceId] = useState(active[0]?.id ?? ""); const [email, setEmail] = useState(""); const [subjectKind, setSubjectKind] = useState<"individual" | "team" | "organization">("individual"); const [subjectId, setSubjectId] = useState("customer-demo"); const [error, setError] = useState<string | null>(null);
  useEffect(() => { if (priceId === "" && active[0] !== undefined) setPriceId(active[0].id); }, [active, priceId]);
  const selected = prices.find((price) => price.id === priceId);
  const submit = async (event: FormEvent) => { event.preventDefault(); if (selected === undefined) return; setError(null); onCheckout(null); try { const returnUrl = commerceCheckoutReturnUrl("success"); const cancelUrl = commerceCheckoutReturnUrl("cancel"); const result = selected.billing.type === "recurring" ? await client.commerce.recurringCheckout({ price_id: selected.id, quantity: 1, subject: { kind: subjectKind, id: subjectId }, customer_email: email.trim() || null, success_url: returnUrl, cancel_url: cancelUrl }) : await client.commerce.oneTimeCheckout({ lines: [{ price_id: selected.id, quantity: 1 }], subject: subjectId.trim() === "" ? null : { kind: subjectKind, id: subjectId }, customer_email: email.trim() || null, client_reference: null, success_url: returnUrl, cancel_url: cancelUrl }); onCheckout(result.url); } catch (cause) { setError(errorMessage(cause)); } };
  return <section className="management-panel span-two"><PanelHeading title="Checkout test" detail="Create a provider-hosted Checkout session with the same API your application uses. FFDB never collects card data." /><form className="price-editor" onSubmit={(event) => void submit(event)}><label className="field"><span>Price</span><select required value={priceId} onChange={(event) => setPriceId(event.target.value)}><option value="">Choose price</option>{active.map((price) => <option key={price.id} value={price.id}>{products.find((product) => product.id === price.product_id)?.name ?? price.id} · {money(price.unit_amount_minor, price.currency)} · {billingLabel(price.billing)}</option>)}</select></label><Field label="Customer email" type="email" value={email} onChange={setEmail} optional /><label className="field"><span>Membership subject</span><select value={subjectKind} onChange={(event) => setSubjectKind(event.target.value as typeof subjectKind)}><option value="individual">Individual</option><option value="team">Team</option><option value="organization">Organization</option></select></label><Field label="Subject ID" value={subjectId} onChange={setSubjectId} optional /><button className="primary-action" disabled={selected === undefined} type="submit">Create Checkout session</button></form>{checkoutUrl === null ? null : <a className="billing-redirect" href={checkoutUrl}>Open hosted Checkout <Icon name="external" size={15} /></a>}{error === null ? null : <div className="access-error" role="alert">{error}</div>}</section>;
}

function CustomerPortalPanel({ account, client }: { readonly account: CommerceAccountSummary | null; readonly client: FFDBClient }) {
  const [kind, setKind] = useState<"individual" | "team" | "organization">("individual");
  const [id, setId] = useState("");
  const [portalUrl, setPortalUrl] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const enabled = account?.capabilities.customer_portal === true;
  const create = async (event: FormEvent) => {
    event.preventDefault();
    setPending(true);
    setError(null);
    setPortalUrl(null);
    try {
      const result = await client.commerce.customerPortal({
        subject: { kind, id },
        return_url: commerceCustomerPortalReturnUrl(),
      });
      setPortalUrl(result.url);
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setPending(false);
    }
  };
  return <section className="management-panel span-two"><PanelHeading title="Customer billing portal" detail="Create a short-lived Stripe portal session for a customer to manage payment methods, invoices, and subscriptions tied to an application subject." /><form className="commerce-inline-form" onSubmit={(event) => void create(event)}><label className="field"><span>Customer subject</span><select disabled={!enabled} value={kind} onChange={(event) => setKind(event.target.value as typeof kind)}><option value="individual">Individual</option><option value="team">Team</option><option value="organization">Organization</option></select></label><Field disabled={!enabled} label="Customer subject ID" value={id} onChange={setId} /><button className="primary-action" disabled={!enabled || pending} type="submit">{pending ? "Preparing…" : "Create customer portal session"}</button></form>{enabled ? null : <div className="commerce-empty">Configure and refresh an enabled payment provider to make customer portal sessions available.</div>}{portalUrl === null ? null : <a className="billing-redirect" href={portalUrl}>Open customer billing portal <Icon name="external" size={15} /></a>}{error === null ? null : <div className="access-error" role="alert">{error}</div>}</section>;
}

function OrdersPanel({ orders, client, onChanged }: { readonly orders: readonly CommerceOrderSummary[]; readonly client: FFDBClient; onChanged(message?: string): void }) {
  const [error, setError] = useState<string | null>(null);
  const update = async (orderId: string, status: CommerceFulfillmentStatus) => { setError(null); try { await client.commerce.updateFulfillment(orderId, status); onChanged("Fulfillment updated"); } catch (cause) { setError(errorMessage(cause)); } };
  return <section className="management-panel span-two"><PanelHeading title="Orders and fulfillment" detail="Orders retain product and price snapshots so historical receipts remain stable when the catalog changes." />{error === null ? null : <div className="access-error" role="alert">{error}</div>}<DataTable headings={["Order", "Status", "Total", "Paid", "Fulfillment"]} rows={orders.map((order) => [<Entity key={order.id} title={order.id} detail={`${order.lines.length} line${order.lines.length === 1 ? "" : "s"}`} />, humanize(order.status), money(order.total_minor, order.currency), order.paid_at_ms === null ? "—" : formatDate(order.paid_at_ms), <FulfillmentSelect key={order.id} order={order} onChange={(status) => void update(order.id, status)} />])} empty="No orders yet." /></section>;
}

function FulfillmentSelect({ order, onChange }: { readonly order: CommerceOrderSummary; onChange(status: CommerceFulfillmentStatus): void }) { const [value, setValue] = useState(order.fulfillment_status); return <span className="fulfillment-control"><select aria-label={`Fulfillment for ${order.id}`} value={value} onChange={(event) => setValue(event.target.value as CommerceFulfillmentStatus)}><option value="unfulfilled">Unfulfilled</option><option value="processing">Processing</option><option value="fulfilled">Fulfilled</option><option value="canceled">Canceled</option></select><button disabled={value === order.fulfillment_status} type="button" onClick={() => onChange(value)}>Update</button></span>; }

function PaymentsPanel({ payments, client, onChanged }: { readonly payments: readonly CommercePaymentSummary[]; readonly client: FFDBClient; onChanged(message?: string): void }) {
  const refundable = payments.filter((payment) => payment.captured_minor > payment.refunded_minor);
  const [paymentId, setPaymentId] = useState(refundable[0]?.id ?? ""); const [amount, setAmount] = useState(""); const [reason, setReason] = useState<"requested_by_customer" | "duplicate" | "fraudulent" | "other">("requested_by_customer"); const [error, setError] = useState<string | null>(null);
  useEffect(() => { if (paymentId === "" && refundable[0] !== undefined) setPaymentId(refundable[0].id); }, [paymentId, refundable]);
  const refund = async (event: FormEvent) => { event.preventDefault(); setError(null); try { await client.commerce.refund({ payment_id: paymentId, amount_minor: amount.trim() === "" ? null : Number(amount), reason }); setAmount(""); onChanged("Refund submitted"); } catch (cause) { setError(errorMessage(cause)); } };
  return <section className="management-panel span-two"><PanelHeading title="Payments and refunds" detail="Refunds are idempotent provider operations and update the local payment and order ledgers from verified results." /><form className="commerce-inline-form" onSubmit={(event) => void refund(event)}><label className="field"><span>Payment</span><select required value={paymentId} onChange={(event) => setPaymentId(event.target.value)}><option value="">Choose captured payment</option>{refundable.map((payment) => <option key={payment.id} value={payment.id}>{payment.id} · {money(payment.captured_minor - payment.refunded_minor, payment.currency)} refundable</option>)}</select></label><Field label="Amount (blank for all)" type="number" value={amount} onChange={setAmount} optional /><label className="field"><span>Reason</span><select value={reason} onChange={(event) => setReason(event.target.value as typeof reason)}><option value="requested_by_customer">Customer request</option><option value="duplicate">Duplicate</option><option value="fraudulent">Fraudulent</option><option value="other">Other</option></select></label><button className="primary-action" disabled={paymentId === ""} type="submit">Submit refund</button></form>{error === null ? null : <div className="access-error" role="alert">{error}</div>}<DataTable headings={["Payment", "Status", "Captured", "Refunded", "Created"]} rows={payments.map((payment) => [payment.id, humanize(payment.status), money(payment.captured_minor, payment.currency), money(payment.refunded_minor, payment.currency), formatDate(payment.created_at_ms)])} empty="No payments yet." /></section>;
}

function SubscriptionsPanel({ subscriptions, client, onChanged }: { readonly subscriptions: readonly CommerceSubscriptionSummary[]; readonly client: FFDBClient; onChanged(message?: string): void }) {
  const [error, setError] = useState<string | null>(null);
  const cancel = async (subscription: CommerceSubscriptionSummary, atPeriodEnd: boolean) => { if (!globalThis.confirm(`${atPeriodEnd ? "Schedule cancellation for" : "Cancel"} subscription ${subscription.id}?`)) return; setError(null); try { await client.commerce.cancelSubscription(subscription.id, { at_period_end: atPeriodEnd }); onChanged(atPeriodEnd ? "Subscription cancellation scheduled" : "Subscription canceled"); } catch (cause) { setError(errorMessage(cause)); } };
  return <section className="management-panel span-two"><PanelHeading title="Subscriptions" detail="Subscription state and access grants are driven by verified provider events, not browser redirects." />{error === null ? null : <div className="access-error" role="alert">{error}</div>}<DataTable headings={["Subscription", "Subject", "Status", "Period ends", "Actions"]} rows={subscriptions.map((subscription) => [subscription.id, `${subscription.subject.kind}:${subscription.subject.id}`, humanize(subscription.status), subscription.current_period_end_ms === null ? "—" : formatDate(subscription.current_period_end_ms), <span className="action-row" key={subscription.id}><button disabled={subscription.status === "canceled"} type="button" onClick={() => void cancel(subscription, true)}>Cancel at period end…</button><button disabled={subscription.status === "canceled"} type="button" onClick={() => void cancel(subscription, false)}>Cancel now…</button></span>])} empty="No subscriptions yet." /></section>;
}

function EntitlementsPanel({ client }: { readonly client: FFDBClient }) {
  const [kind, setKind] = useState<"individual" | "team" | "organization">("individual"); const [id, setId] = useState(""); const [results, setResults] = useState<readonly CommerceEntitlementSummary[] | null>(null); const [error, setError] = useState<string | null>(null);
  const inspect = async (event: FormEvent) => { event.preventDefault(); setError(null); try { setResults(await client.commerce.entitlements({ kind, id })); } catch (cause) { setError(errorMessage(cause)); } };
  return <section className="management-panel span-two"><PanelHeading title="Entitlement inspector" detail="Resolve the effective access granted to an individual, team, or organization at the current time." /><form className="commerce-inline-form" onSubmit={(event) => void inspect(event)}><label className="field"><span>Subject kind</span><select value={kind} onChange={(event) => setKind(event.target.value as typeof kind)}><option value="individual">Individual</option><option value="team">Team</option><option value="organization">Organization</option></select></label><Field label="Subject ID" value={id} onChange={setId} /><button className="primary-action" type="submit">Inspect entitlements</button></form>{error === null ? null : <div className="access-error" role="alert">{error}</div>}{results === null ? <div className="commerce-empty">Enter a subject to inspect its effective access.</div> : <DataTable headings={["Key", "Value", "Source", "Valid until"]} rows={results.map((item) => [item.key, entitlementLabel(item.value), item.subscription_id ?? item.order_id ?? "—", item.valid_until_ms === null ? "No expiry" : formatDate(item.valid_until_ms)])} empty="No active entitlements." />}</section>;
}

function PanelHeading({ title, detail }: { readonly title: string; readonly detail: string }) { return <div className="management-header"><div><h2>{title}</h2><p>{detail}</p></div></div>; }
function Field({ label, value, onChange, type = "text", optional = false, disabled = false }: { readonly label: string; readonly value: string; readonly type?: string; readonly optional?: boolean; readonly disabled?: boolean; onChange(value: string): void }) { return <label className="field"><span>{label}</span><input disabled={disabled} required={!optional} type={type} value={value} onChange={(event) => onChange(event.target.value)} /></label>; }
function Capability({ enabled, label }: { readonly enabled: boolean; readonly label: string }) { return <span className={enabled ? "commerce-capability enabled" : "commerce-capability"}>{enabled ? "✓" : "–"} {label}</span>; }
function Entity({ title, detail }: { readonly title: string; readonly detail: string }) { return <span className="entity-primary"><strong>{title}</strong><small>{detail}</small></span>; }
function DataTable({ headings, rows, empty }: { readonly headings: readonly string[]; readonly rows: readonly (readonly ReactNode[])[]; readonly empty: string }) { return <ManagedTable empty={empty} headings={headings} label="records" rows={rows} />; }
function CommerceError({ message, retry }: { readonly message: string; retry(): void }) { return <div className="error-state" role="alert"><strong>Project commerce is unavailable</strong><span>{message}</span><button type="button" onClick={retry}>Try again</button></div>; }

function handleProviderTabKeyDown(event: KeyboardEvent<HTMLButtonElement>, current: "byo" | "connect", onSelect: (mode: "byo" | "connect") => void) {
  if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
  event.preventDefault();
  const next = event.key === "Home" || event.key === "ArrowLeft" ? "byo" : event.key === "End" || event.key === "ArrowRight" ? "connect" : current;
  onSelect(next);
  const index = next === "byo" ? 0 : 1;
  event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role='tab']").item(index).focus();
}

function initialTaskForView(view: CommerceView): CommerceTask {
  if (view === "orders") return "orders";
  if (view === "subscriptions") return "subscriptions";
  return "catalog";
}

function tasksForView(view: CommerceView, data: CommerceData): readonly CommerceTaskDefinition[] {
  if (view === "orders") return [
    { id: "orders", label: "Fulfillment", count: data.orders.length },
    { id: "payments", label: "Refunds", count: data.payments.length },
  ];
  if (view === "subscriptions") return [
    { id: "subscriptions", label: "Lifecycle", count: data.subscriptions.length },
    { id: "entitlements", label: "Entitlements" },
  ];
  return [
    { id: "catalog", label: "Catalog & prices", count: data.products.length + data.prices.length },
    { id: "checkout", label: "Checkout" },
    { id: "customer-portal", label: "Customer portal" },
    { id: "provider", label: "Provider" },
  ];
}

function commerceViewTitle(view: CommerceView): string {
  if (view === "orders") return "Orders & payments";
  if (view === "subscriptions") return "Subscriptions & access";
  return "Catalog & checkout";
}

function commerceReturnUrl(state: "return" | "refresh"): string { const url = new URL(import.meta.env.BASE_URL, globalThis.location.origin); url.searchParams.set("commerce-connect", state); return url.href; }
function commerceCheckoutReturnUrl(state: "success" | "cancel"): string { const url = new URL(import.meta.env.BASE_URL, globalThis.location.origin); url.searchParams.set("commerce-checkout", state); return url.href; }
function commerceCustomerPortalReturnUrl(): string { const url = new URL(import.meta.env.BASE_URL, globalThis.location.origin); url.searchParams.set("commerce-customer-portal", "return"); return url.href; }
function entitlementFromInput(value: string): CommerceEntitlementValue { const normalized = value.trim().toLowerCase(); if (normalized === "true" || normalized === "false") return { type: "enabled", value: normalized === "true" }; const quantity = Number(value); return Number.isSafeInteger(quantity) && quantity >= 0 ? { type: "quantity", value: quantity } : { type: "text", value }; }
function entitlementLabel(value: CommerceEntitlementValue): string { return value.type === "enabled" ? (value.value ? "Enabled" : "Disabled") : String(value.value); }
function billingLabel(value: CommercePriceBilling): string { return value.type === "one_time" ? "One time" : `Every ${value.interval_count === 1 ? "" : `${value.interval_count} `}${value.interval}${value.interval_count === 1 ? "" : "s"}`; }
function humanize(value: string): string { return value.replaceAll("_", " ").replace(/^./u, (character) => character.toUpperCase()); }
function money(amount: number, currency: string): string { try { return new Intl.NumberFormat(undefined, { style: "currency", currency: currency.toUpperCase() }).format(amount / 100); } catch { return `${amount} ${currency.toUpperCase()}`; } }
function formatDate(value: number): string { return new Date(value).toLocaleDateString(); }
function errorMessage(cause: unknown): string { return cause instanceof Error ? cause.message : "The request could not be completed."; }
