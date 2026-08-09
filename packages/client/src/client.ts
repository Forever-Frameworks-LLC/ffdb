import type {
  ApiKeySummary,
  AcceptOrganizationInvitationRequest,
  AddOrganizationMemberRequest,
  AuditLogEntry,
  AuthUser,
  AuthTokenPair,
  AuthSettings,
  BackupResult,
  BackupSummary,
  BillingRedirect,
  CompleteInstanceSetupRequest,
  CompleteInstanceSetupResponse,
  CreatedApiKey,
  CreateOrganizationInvitationRequest,
  CreatePlatformCheckoutRequest,
  DeveloperScope,
  DeveloperSession,
  EmailTemplateKind,
  EmailTemplateArtifactInput,
  EmailTemplatePreview,
  EmailTemplateVersion,
  ErrorEnvelope,
  HealthStatus,
  GrantOrganizationBillingExemptionRequest,
  InstanceAdministratorSummary,
  InstanceBillingOnboarding,
  InstanceOrganizationPage,
  InstanceOrganizationSummary,
  InstancePlanCatalogEntry,
  InstanceStatus,
  InstanceUserPage,
  InstanceUserSummary,
  OrganizationBillingExemptionSummary,
  OrganizationCreationPolicy,
  JsonValue,
  MigrationSpec,
  MigrationSummary,
  ObservabilityRange,
  ObservabilitySummary,
  MultipartPart,
  MultipartUpload,
  OrganizationSummary,
  OrganizationMembershipSummary,
  PlatformErrorBody,
  PlatformBillingSummary,
  PlatformInvoiceSummary,
  PlatformUsageSummary,
  PolicyDefinition,
  ProjectPaymentsSummary,
  ProjectSummary,
  QueryRequest,
  QueryResult,
  RegisterRequest,
  RegisterResponse,
  RequestOptions,
  PublicInstanceSetupStatus,
  PutInstancePlanCatalogEntryRequest,
  CreateInstanceConnectOnboardingRequest,
  RestoreResult,
  SchemaSnapshot,
  SessionSummary,
  SignObjectRequest,
  SignedObjectRequest,
  SnapshotResponse,
  StorageBucket,
  StorageBucketRequest,
  StorageObjectPage,
  SyncPullResponse,
  SyncPushRequest,
  SyncPushResponse,
  TransactionRequest,
  UpdateOrganizationMemberRequest,
} from "./types.js";
import type {
  CancelCommerceSubscriptionRequest,
  CommerceAccountSummary,
  CommerceCheckoutResponse,
  CommerceEntitlementSummary,
  CommerceFulfillmentStatus,
  CommerceOnboardingResponse,
  CommerceOrderSummary,
  CommercePaymentSummary,
  CommercePriceSummary,
  CommerceProductSummary,
  CommerceRefundSummary,
  CommerceSubscriptionSummary,
  ConfigureCommerceByoRequest,
  CreateCommerceConnectOnboardingRequest,
  CreateCommerceCustomerPortalRequest,
  CreateCommercePriceRequest,
  CreateCommerceProductRequest,
  CreateCommerceRefundRequest,
  CreateOneTimeCommerceCheckoutRequest,
  CreateRecurringCommerceCheckoutRequest,
} from "./types.js";

export interface SessionStore {
  get(): Promise<AuthTokenPair | null>;
  set(session: AuthTokenPair | null): Promise<void>;
}

export interface DeveloperSessionStore {
  get(): Promise<DeveloperSession | null>;
  set(session: DeveloperSession | null): Promise<void>;
}

export class MemorySessionStore implements SessionStore {
  readonly #key: string;
  static readonly #sessions = new Map<string, AuthTokenPair>();

  constructor(key = "default") {
    this.#key = key;
  }

  async get(): Promise<AuthTokenPair | null> {
    return MemorySessionStore.#sessions.get(this.#key) ?? null;
  }

  async set(session: AuthTokenPair | null): Promise<void> {
    if (session === null) {
      MemorySessionStore.#sessions.delete(this.#key);
    } else {
      MemorySessionStore.#sessions.set(this.#key, session);
    }
  }
}

export class BrowserSessionStore implements SessionStore {
  readonly #key: string;
  readonly #storage: Storage;

  constructor(storage: Storage, key = "ffdb.session") {
    this.#storage = storage;
    this.#key = key;
  }

  async get(): Promise<AuthTokenPair | null> {
    const value = this.#storage.getItem(this.#key);
    if (value === null) return null;
    try {
      return JSON.parse(value) as AuthTokenPair;
    } catch {
      this.#storage.removeItem(this.#key);
      return null;
    }
  }

  async set(session: AuthTokenPair | null): Promise<void> {
    if (session === null) this.#storage.removeItem(this.#key);
    else this.#storage.setItem(this.#key, JSON.stringify(session));
  }
}

export class MemoryDeveloperSessionStore implements DeveloperSessionStore {
  readonly #key: string;
  static readonly #sessions = new Map<string, DeveloperSession>();

  constructor(key = "platform") { this.#key = key; }
  async get(): Promise<DeveloperSession | null> { return MemoryDeveloperSessionStore.#sessions.get(this.#key) ?? null; }
  async set(session: DeveloperSession | null): Promise<void> { if (session === null) MemoryDeveloperSessionStore.#sessions.delete(this.#key); else MemoryDeveloperSessionStore.#sessions.set(this.#key, session); }
}

export class BrowserDeveloperSessionStore implements DeveloperSessionStore {
  constructor(private readonly storage: Storage, private readonly key = "ffdb.developer-session") {}
  async get(): Promise<DeveloperSession | null> { const value = this.storage.getItem(this.key); if (value === null) return null; try { return JSON.parse(value) as DeveloperSession; } catch { this.storage.removeItem(this.key); return null; } }
  async set(session: DeveloperSession | null): Promise<void> { if (session === null) this.storage.removeItem(this.key); else this.storage.setItem(this.key, JSON.stringify(session)); }
}

export interface FFDBClientOptions {
  readonly baseUrl: string;
  readonly projectId?: string;
  readonly developerKey?: string;
  readonly fetch?: typeof globalThis.fetch;
  readonly sessionStore?: SessionStore;
  readonly developerSessionStore?: DeveloperSessionStore;
}

export class FFDBError extends Error {
  readonly code: string;
  readonly requestId: string | null;
  readonly status: number;
  readonly details: Readonly<Record<string, JsonValue>>;

  constructor(status: number, error: PlatformErrorBody) {
    super(error.message);
    this.name = "FFDBError";
    this.status = status;
    this.code = error.code;
    this.requestId = error.request_id || null;
    this.details = error.details ?? {};
  }
}

type CredentialMode = "developer" | "platform" | "admin" | "user" | "either" | "none";

interface InternalRequestOptions extends RequestOptions {
  readonly credential?: CredentialMode;
  readonly method?: string;
  readonly body?: BodyInit;
  readonly headers?: HeadersInit;
  readonly attempt?: number;
  readonly authRetried?: boolean;
  /** Internal-only: the server operation has a durable replay receipt. */
  readonly replaySafe?: boolean;
}

const MAX_REQUEST_ATTEMPTS = 3;
const MAX_RETRY_DELAY_MS = 5_000;

export class FFDBClient {
  readonly baseUrl: string;
  #projectId: string;
  #developerKey: string | null;
  readonly #fetch: typeof globalThis.fetch;
  readonly #sessionStore: SessionStore;
  readonly #developerSessionStore: DeveloperSessionStore;
  #session: AuthTokenPair | null = null;
  #developerSession: DeveloperSession | null = null;
  #loaded = false;
  #developerLoaded = false;
  #refreshing: Promise<AuthTokenPair> | null = null;
  readonly auth: AuthClient;
  readonly storage: StorageClient;
  readonly sync: SyncClient;
  readonly commerce: CommerceClient;

  constructor(options: FFDBClientOptions) {
    const baseUrl = new URL(options.baseUrl);
    if (!/^https?:$/.test(baseUrl.protocol)) throw new TypeError("baseUrl must use HTTP(S)");
    if (baseUrl.username !== "" || baseUrl.password !== "") {
      throw new TypeError("baseUrl must not contain user information");
    }
    if (baseUrl.search !== "" || baseUrl.hash !== "") {
      throw new TypeError("baseUrl must not contain a query string or fragment");
    }
    this.baseUrl = baseUrl.href.replace(/\/$/, "");
    this.#projectId = options.projectId ?? "";
    this.#developerKey = options.developerKey ?? null;
    if (options.fetch !== undefined) {
      this.#fetch = options.fetch;
    } else {
      const runtimeFetch = globalThis.fetch;
      if (typeof runtimeFetch !== "function") {
        throw new TypeError("A fetch implementation is required in this runtime");
      }
      this.#fetch = runtimeFetch.bind(globalThis);
    }
    this.#sessionStore = options.sessionStore ?? new MemorySessionStore(options.projectId ?? "platform");
    this.#developerSessionStore = options.developerSessionStore ?? new MemoryDeveloperSessionStore();
    this.auth = new AuthClient(this);
    this.storage = new StorageClient(this);
    this.sync = new SyncClient(this);
    this.commerce = new CommerceClient(this);
  }

  get projectId(): string { return this.#projectId; }

  setProjectId(projectId: string): void {
    this.#projectId = projectId;
  }

  setDeveloperKey(developerKey: string | null): void {
    this.#developerKey = developerKey;
  }

  async query<Row extends readonly (null | number | string | { readonly $blob: string })[] = readonly (
    | null
    | number
    | string
    | { readonly $blob: string }
  )[]>(request: QueryRequest, options: RequestOptions = {}): Promise<QueryResult<Row>> {
    return this.workerRequest<QueryResult<Row>>(this.projectPath("query"), {
      method: "POST",
      body: JSON.stringify(request),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("query"),
      credential: "either",
    });
  }

  async transaction(
    request: TransactionRequest,
    options: RequestOptions = {},
  ): Promise<readonly QueryResult[]> {
    return this.workerRequest<readonly QueryResult[]>(this.projectPath("transaction"), {
      method: "POST",
      body: JSON.stringify(request),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("transaction"),
      credential: "either",
    });
  }

  async migrate(spec: MigrationSpec, options: RequestOptions = {}): Promise<JsonValue> {
    return this.workerRequest(this.projectPath("migrations"), {
      method: "POST",
      body: JSON.stringify(spec),
      ...options,
      idempotencyKey: options.idempotencyKey ?? `migration:${spec.id}:${spec.checksum}`,
      credential: "developer",
    });
  }

  async rollbackMigration(id: string, options: RequestOptions = {}): Promise<JsonValue> {
    return this.workerRequest(this.projectPath(`migrations/${encodeURIComponent(id)}/rollback`), {
      method: "POST",
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("migration-rollback"),
      credential: "developer",
    });
  }

  async schema(options: RequestOptions = {}): Promise<SchemaSnapshot> {
    return this.workerRequest(this.projectPath("schema"), { ...options, credential: "developer" });
  }

  async policies(options: RequestOptions = {}): Promise<readonly PolicyDefinition[]> {
    return this.workerRequest(this.projectPath("policies"), { ...options, credential: "developer" });
  }

  async health(options: RequestOptions = {}): Promise<HealthStatus> {
    return this.request("/healthz", { ...options, credential: "none" });
  }

  async readiness(options: RequestOptions = {}): Promise<HealthStatus> {
    return this.request("/readyz", { ...options, credential: "none" });
  }

  async instanceSetupStatus(options: RequestOptions = {}): Promise<PublicInstanceSetupStatus> {
    return this.request("/v1/instance/setup/status", { ...options, credential: "none" });
  }

  async developerBootstrap(
    bootstrapToken: string,
    email: string,
    password: string,
    options: RequestOptions = {},
  ): Promise<DeveloperSession> {
    const session = await this.request<DeveloperSession>("/v1/developer/bootstrap", {
      method: "POST",
      body: JSON.stringify({ email, password }),
      headers: { "x-ffdb-bootstrap-token": bootstrapToken },
      ...options,
      credential: "none",
      retry: false,
    });
    this.#developerSession = session;
    this.#developerLoaded = true;
    await this.#developerSessionStore.set(session);
    return session;
  }

  async instanceStatus(options: RequestOptions = {}): Promise<InstanceStatus> {
    return this.request("/v1/instance", { ...options, credential: "platform" });
  }

  async configureInstance(
    input: CompleteInstanceSetupRequest,
    options: RequestOptions = {},
  ): Promise<CompleteInstanceSetupResponse> {
    return this.request("/v1/instance", {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("instance-configure"),
      credential: "platform",
    });
  }

  async createInstanceConnectOnboarding(
    input: CreateInstanceConnectOnboardingRequest,
    options: RequestOptions = {},
  ): Promise<InstanceBillingOnboarding> {
    return this.request("/v1/instance/billing/connect/onboarding", {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("instance-connect"),
      credential: "platform",
    });
  }

  async refreshInstanceBilling(options: RequestOptions = {}): Promise<InstanceStatus> {
    return this.request("/v1/instance/billing/refresh", {
      method: "POST",
      ...options,
      credential: "platform",
    });
  }

  async updateOrganizationCreationPolicy(
    policy: OrganizationCreationPolicy,
    options: RequestOptions = {},
  ): Promise<InstanceStatus> {
    return this.request("/v1/instance/organization-creation-policy", {
      method: "PATCH",
      body: JSON.stringify({ organization_creation_policy: policy }),
      ...options,
      credential: "platform",
    });
  }

  async instanceAdministrators(
    options: RequestOptions = {},
  ): Promise<readonly InstanceAdministratorSummary[]> {
    return this.request("/v1/instance/administrators", { ...options, credential: "platform" });
  }

  async grantInstanceAdministrator(
    userId: string,
    options: RequestOptions = {},
  ): Promise<InstanceAdministratorSummary> {
    return this.request("/v1/instance/administrators", {
      method: "POST",
      body: JSON.stringify({ user_id: userId }),
      ...options,
      credential: "platform",
    });
  }

  async revokeInstanceAdministrator(
    userId: string,
    options: RequestOptions = {},
  ): Promise<void> {
    return this.request(`/v1/instance/administrators/${encodeURIComponent(userId)}`, {
      method: "DELETE",
      ...options,
      credential: "platform",
    });
  }

  async instanceOrganizations(
    page: { readonly limit?: number; readonly offset?: number } = {},
    options: RequestOptions = {},
  ): Promise<InstanceOrganizationPage> {
    return this.request(`/v1/instance/organizations${pageQuery(page)}`, {
      ...options,
      credential: "platform",
    });
  }

  async instanceUsers(
    page: { readonly limit?: number; readonly offset?: number } = {},
    options: RequestOptions = {},
  ): Promise<InstanceUserPage> {
    return this.request(`/v1/instance/users${pageQuery(page)}`, {
      ...options,
      credential: "platform",
    });
  }

  async setInstanceOrganizationDisabled(
    organizationId: string,
    disabled: boolean,
    options: RequestOptions = {},
  ): Promise<InstanceOrganizationSummary> {
    return this.request(
      `/v1/instance/organizations/${encodeURIComponent(organizationId)}`,
      {
        method: "PATCH",
        body: JSON.stringify({ disabled }),
        ...options,
        credential: "platform",
      },
    );
  }

  async setInstanceUserDisabled(
    userId: string,
    disabled: boolean,
    options: RequestOptions = {},
  ): Promise<InstanceUserSummary> {
    return this.request(`/v1/instance/users/${encodeURIComponent(userId)}`, {
      method: "PATCH",
      body: JSON.stringify({ disabled }),
      ...options,
      credential: "platform",
    });
  }

  async billingExemptions(
    options: RequestOptions = {},
  ): Promise<readonly OrganizationBillingExemptionSummary[]> {
    return this.request("/v1/instance/billing-exemptions", {
      ...options,
      credential: "platform",
    });
  }

  async grantBillingExemption(
    organizationId: string,
    reason: string,
    options: RequestOptions = {},
  ): Promise<OrganizationBillingExemptionSummary> {
    const input: GrantOrganizationBillingExemptionRequest = { reason };
    return this.request(
      `/v1/instance/billing-exemptions/${encodeURIComponent(organizationId)}`,
      {
        method: "PUT",
        body: JSON.stringify(input),
        ...options,
        credential: "platform",
      },
    );
  }

  async revokeBillingExemption(
    organizationId: string,
    options: RequestOptions = {},
  ): Promise<void> {
    return this.request(
      `/v1/instance/billing-exemptions/${encodeURIComponent(organizationId)}`,
      { method: "DELETE", ...options, credential: "platform" },
    );
  }

  async instancePlans(options: RequestOptions = {}): Promise<readonly InstancePlanCatalogEntry[]> {
    return this.request("/v1/instance/plans", { ...options, credential: "platform" });
  }

  async putInstancePlan(
    tier: InstancePlanCatalogEntry["tier"],
    input: PutInstancePlanCatalogEntryRequest,
    options: RequestOptions = {},
  ): Promise<InstancePlanCatalogEntry> {
    return this.request(`/v1/instance/plans/${encodeURIComponent(tier)}`, {
      method: "PUT",
      body: JSON.stringify(input),
      ...options,
      credential: "platform",
    });
  }

  async retireInstancePlan(
    tier: InstancePlanCatalogEntry["tier"],
    options: RequestOptions = {},
  ): Promise<InstancePlanCatalogEntry> {
    return this.request(`/v1/instance/plans/${encodeURIComponent(tier)}`, {
      method: "DELETE",
      ...options,
      credential: "platform",
    });
  }

  async metrics(options: RequestOptions = {}): Promise<string> {
    const response = await this.#fetch(`${this.baseUrl}/metrics`, {
      headers: { Accept: "text/plain" },
      ...(options.signal === undefined ? {} : { signal: options.signal }),
    });
    if (!response.ok) {
      throw localError("metrics.unavailable", "Metrics endpoint is unavailable", response.status);
    }
    return readBoundedText(response, 512 * 1024, "metrics.response_too_large");
  }

  async projectObservability(
    range: ObservabilityRange = "24h",
    options: RequestOptions = {},
  ): Promise<ObservabilitySummary> {
    const query = new URLSearchParams({ range });
    return this.request(`${this.projectPath("observability")}?${query}`, {
      ...options,
      credential: "platform",
    });
  }

  async instanceObservability(
    range: ObservabilityRange = "24h",
    projectId?: string,
    options: RequestOptions = {},
  ): Promise<ObservabilitySummary> {
    const query = new URLSearchParams({ range });
    if (projectId !== undefined && projectId !== "") query.set("project_id", projectId);
    return this.request(`/v1/instance/observability?${query}`, {
      ...options,
      credential: "platform",
    });
  }

  async organizations(options: RequestOptions = {}): Promise<readonly OrganizationSummary[]> {
    return this.request("/v1/organizations", { ...options, credential: "platform" });
  }

  async createOrganization(
    input: { readonly name: string; readonly slug: string },
    options: RequestOptions = {},
  ): Promise<OrganizationSummary> {
    return this.request("/v1/organizations", {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      credential: "platform",
    });
  }

  async organizationMembers(
    organizationId: string,
    options: RequestOptions = {},
  ): Promise<readonly OrganizationMembershipSummary[]> {
    return this.request(`/v1/organizations/${encodeURIComponent(organizationId)}/members`, {
      ...options,
      credential: "platform",
    });
  }

  async addOrganizationMember(
    organizationId: string,
    input: AddOrganizationMemberRequest,
    options: RequestOptions = {},
  ): Promise<OrganizationMembershipSummary> {
    return this.request(`/v1/organizations/${encodeURIComponent(organizationId)}/members`, {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      credential: "platform",
    });
  }

  async updateOrganizationMember(
    organizationId: string,
    userId: string,
    input: UpdateOrganizationMemberRequest,
    options: RequestOptions = {},
  ): Promise<OrganizationMembershipSummary> {
    return this.request(`/v1/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(userId)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
      ...options,
      credential: "platform",
    });
  }

  async removeOrganizationMember(
    organizationId: string,
    userId: string,
    options: RequestOptions = {},
  ): Promise<void> {
    return this.request(`/v1/organizations/${encodeURIComponent(organizationId)}/members/${encodeURIComponent(userId)}`, {
      method: "DELETE",
      ...options,
      credential: "platform",
    });
  }

  async createOrganizationInvitation(
    organizationId: string,
    input: CreateOrganizationInvitationRequest,
    options: RequestOptions = {},
  ): Promise<void> {
    return this.request(`/v1/organizations/${encodeURIComponent(organizationId)}/invitations`, {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      credential: "platform",
    });
  }

  async acceptOrganizationInvitation(
    input: AcceptOrganizationInvitationRequest,
    options: RequestOptions = {},
  ): Promise<DeveloperSession> {
    const session = await this.request<DeveloperSession>("/v1/developer/invitations/accept", {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      credential: "none",
      retry: false,
    });
    this.#developerSession = session;
    this.#developerLoaded = true;
    await this.#developerSessionStore.set(session);
    return session;
  }

  async projects(organizationId: string, options: RequestOptions = {}): Promise<readonly ProjectSummary[]> {
    return this.request(`/v1/organizations/${encodeURIComponent(organizationId)}/projects`, {
      ...options,
      credential: "platform",
    });
  }

  async createProject(
    input: {
      readonly organization_id: string;
      readonly name: string;
      readonly slug: string;
      readonly region?: string;
    },
    options: RequestOptions = {},
  ): Promise<ProjectSummary> {
    return this.request("/v1/projects", {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? `project:${input.organization_id}:${input.slug}`,
      credential: "platform",
    });
  }

  async organizationBilling(
    organizationId: string,
    options: RequestOptions = {},
  ): Promise<PlatformBillingSummary> {
    return this.request(`/v1/organizations/${encodeURIComponent(organizationId)}/billing`, {
      ...options,
      credential: "platform",
    });
  }

  async createBillingCheckout(
    organizationId: string,
    input: CreatePlatformCheckoutRequest,
    options: RequestOptions = {},
  ): Promise<BillingRedirect> {
    return this.request(
      `/v1/organizations/${encodeURIComponent(organizationId)}/billing/checkout`,
      {
        method: "POST",
        body: JSON.stringify(input),
        ...options,
        idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("billing-checkout"),
        credential: "platform",
      },
    );
  }

  async createBillingPortal(
    organizationId: string,
    options: RequestOptions = {},
  ): Promise<BillingRedirect> {
    return this.request(
      `/v1/organizations/${encodeURIComponent(organizationId)}/billing/portal`,
      {
        method: "POST",
        body: JSON.stringify({}),
        ...options,
        idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("billing-portal"),
        credential: "platform",
      },
    );
  }

  async organizationInvoices(
    organizationId: string,
    options: RequestOptions = {},
  ): Promise<readonly PlatformInvoiceSummary[]> {
    return this.request(
      `/v1/organizations/${encodeURIComponent(organizationId)}/billing/invoices`,
      {
        ...options,
        credential: "platform",
      },
    );
  }

  async organizationUsage(
    organizationId: string,
    options: RequestOptions = {},
  ): Promise<PlatformUsageSummary> {
    return this.request(
      `/v1/organizations/${encodeURIComponent(organizationId)}/billing/usage`,
      {
        ...options,
        credential: "platform",
      },
    );
  }

  /**
   * Returns the compact project-commerce capability summary used by older
   * management clients. New integrations should use `client.commerce` for the
   * complete account, catalog, Checkout, orders and subscriptions API.
   */
  async projectPayments(options: RequestOptions = {}): Promise<ProjectPaymentsSummary> {
    return this.request(this.projectPath("payments"), {
      ...options,
      credential: "platform",
    });
  }

  async createApiKey(
    input: {
      readonly name: string;
      readonly scopes: readonly DeveloperScope[];
      readonly expires_at_ms: number | null;
    },
    options: RequestOptions = {},
  ): Promise<CreatedApiKey> {
    return this.request(this.projectPath("api-keys"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      credential: "platform",
    });
  }

  async apiKeys(options: RequestOptions = {}): Promise<readonly ApiKeySummary[]> {
    return this.request(this.projectPath("api-keys"), { ...options, credential: "platform" });
  }

  async revokeApiKey(id: string, options: RequestOptions = {}): Promise<void> {
    return this.request(this.projectPath(`api-keys/${encodeURIComponent(id)}/revoke`), {
      method: "POST",
      ...options,
      credential: "platform",
    });
  }

  async rotateSigningKey(options: RequestOptions = {}): Promise<{
    readonly active_kid: string;
    readonly previous_kid: string | null;
    readonly previous_valid_until_seconds: number | null;
  }> {
    return this.request(this.projectPath("keys/rotate"), {
      method: "POST", ...options, credential: "platform",
    });
  }

  async migrationHistory(options: RequestOptions = {}): Promise<readonly MigrationSummary[]> {
    return this.request(this.projectPath("migrations"), { ...options, credential: "developer" });
  }

  async seed(sql: string, options: RequestOptions = {}): Promise<JsonValue> {
    return this.request(this.projectPath("seed"), {
      method: "POST",
      body: JSON.stringify({ sql }),
      ...options,
      credential: "developer",
    });
  }

  async authSettings(options: RequestOptions = {}): Promise<AuthSettings> {
    return this.request(this.projectPath("auth/settings"), { ...options, credential: "developer" });
  }

  async updateAuthSettings(
    settings: Partial<AuthSettings>,
    options: RequestOptions = {},
  ): Promise<AuthSettings> {
    return this.request(this.projectPath("auth/settings"), {
      method: "PATCH",
      body: JSON.stringify(settings),
      ...options,
      credential: "developer",
    });
  }

  async authUsers(options: RequestOptions = {}): Promise<readonly AuthUser[]> {
    return this.request(this.projectPath("auth/users"), { ...options, credential: "developer" });
  }

  async setAuthUserDisabled(
    userId: string,
    disabled: boolean,
    options: RequestOptions = {},
  ): Promise<void> {
    return this.request(this.projectPath(`auth/users/${encodeURIComponent(userId)}`), {
      method: "PATCH",
      body: JSON.stringify({ disabled }),
      ...options,
      credential: "developer",
    });
  }

  async logs(options: RequestOptions & { readonly limit?: number } = {}): Promise<readonly AuditLogEntry[]> {
    const query = new URLSearchParams();
    if (options.limit !== undefined) query.set("limit", String(options.limit));
    const suffix = query.size === 0 ? "" : `?${query}`;
    return this.request(`${this.projectPath("logs")}${suffix}`, { ...options, credential: "developer" });
  }

  async backups(options: RequestOptions = {}): Promise<readonly BackupSummary[]> {
    return this.request(this.projectPath("backups"), { ...options, credential: "developer" });
  }

  async createBackup(options: RequestOptions = {}): Promise<BackupResult> {
    return this.workerRequest(this.projectPath("backups"), {
      method: "POST",
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("backup-create"),
      credential: "developer",
    });
  }

  async restoreBackup(id: string, options: RequestOptions = {}): Promise<RestoreResult> {
    return this.workerRequest(this.projectPath(`backups/${encodeURIComponent(id)}/restore`), {
      method: "POST",
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("backup-restore"),
      credential: "developer",
    });
  }

  async integrityCheck(options: RequestOptions = {}): Promise<{ readonly ok: boolean; readonly messages: readonly string[] }> {
    return this.workerRequest(this.projectPath("integrity-check"), {
      method: "POST",
      ...options,
      credential: "developer",
    });
  }

  async emailTemplates(
    kind?: EmailTemplateKind,
    options: RequestOptions = {},
  ): Promise<readonly EmailTemplateVersion[]> {
    const suffix = kind === undefined ? "email/templates" : `email/templates?kind=${kind}`;
    return this.request(this.projectPath(suffix), { ...options, credential: "developer" });
  }

  async importEmailTemplateArtifact(
    artifact: EmailTemplateArtifactInput,
    options: RequestOptions = {},
  ): Promise<EmailTemplateVersion> {
    return this.request(this.projectPath("email/templates/artifacts"), {
      method: "POST",
      body: JSON.stringify(artifact),
      ...options,
      credential: "developer",
    });
  }

  async publishEmailTemplate(
    kind: EmailTemplateKind,
    version: number,
    options: RequestOptions = {},
  ): Promise<EmailTemplateVersion> {
    return this.request(this.projectPath(`email/templates/${kind}/${version}/publish`), {
      method: "POST",
      ...options,
      credential: "developer",
    });
  }

  async previewEmailTemplate(
    kind: EmailTemplateKind,
    version: number,
    variables: Readonly<Record<string, string | number | boolean>>,
    options: RequestOptions = {},
  ): Promise<EmailTemplatePreview> {
    return this.request(this.projectPath(`email/templates/${kind}/${version}/preview`), {
      method: "POST",
      body: JSON.stringify({ variables }),
      ...options,
      credential: "developer",
    });
  }

  projectPath(path: string): string {
    if (this.#projectId === "") throw localError("project.missing", "Project id required");
    return `/v1/projects/${encodeURIComponent(this.#projectId)}/${path}`;
  }

  async developerSignIn(email: string, password: string, options: RequestOptions = {}): Promise<DeveloperSession> {
    const session = await this.request<DeveloperSession>("/v1/developer/sign-in", {
      method: "POST", body: JSON.stringify({ email, password }), ...options, credential: "none", retry: false,
    });
    this.#developerSession = session; this.#developerLoaded = true; await this.#developerSessionStore.set(session); return session;
  }

  async developerSession(): Promise<DeveloperSession | null> { await this.#loadDeveloperSession(); return this.#developerSession; }

  async refreshDeveloperSession(options: RequestOptions = {}): Promise<DeveloperSession> {
    await this.#loadDeveloperSession();
    if (this.#developerSession === null) throw localError("developer.session_missing", "Developer session required");
    const session = await this.request<DeveloperSession>("/v1/developer/refresh", {
      method: "POST", body: JSON.stringify({ session_token: this.#developerSession.session_token }), ...options, credential: "none", retry: false,
    });
    this.#developerSession = session; await this.#developerSessionStore.set(session); return session;
  }

  async developerSignOut(options: RequestOptions = {}): Promise<void> {
    await this.#loadDeveloperSession();
    try {
      if (this.#developerSession !== null) await this.request("/v1/developer/sign-out", {
        method: "POST", body: JSON.stringify({ session_token: this.#developerSession.session_token }), ...options, credential: "none", retry: false,
      });
    } finally {
      // A remote session may already be expired or the instance may be offline.
      // Signing out must still remove the unusable browser-local credential.
      this.#developerSession = null; this.#developerLoaded = true; await this.#developerSessionStore.set(null);
    }
  }

  async currentSession(): Promise<AuthTokenPair | null> {
    await this.#loadSession();
    return this.#session;
  }

  async setSession(session: AuthTokenPair | null): Promise<void> {
    this.#session = session;
    this.#loaded = true;
    await this.#sessionStore.set(session);
  }

  async refreshSession(signal?: AbortSignal): Promise<AuthTokenPair> {
    if (this.#refreshing !== null) return this.#refreshing;
    this.#refreshing = this.#performRefresh(signal).finally(() => {
      this.#refreshing = null;
    });
    return this.#refreshing;
  }

  async #performRefresh(signal?: AbortSignal): Promise<AuthTokenPair> {
    await this.#loadSession();
    if (this.#session === null) throw localError("auth.session_missing", "No session to refresh");
    const session = await this.request<AuthTokenPair>(this.projectPath("auth/refresh"), {
      method: "POST",
      body: JSON.stringify({ refresh_token: this.#session.refresh_token }),
      ...(signal === undefined ? {} : { signal }),
      credential: "none",
      retry: false,
    });
    await this.setSession(session);
    return session;
  }

  async request<T>(path: string, options: InternalRequestOptions = {}): Promise<T> {
    await Promise.all([this.#loadSession(), this.#loadDeveloperSession()]);
    const credential = options.credential ?? "either";
    const headers = new Headers(options.headers);
    headers.set("Accept", "application/json");
    if (options.body !== undefined) headers.set("Content-Type", "application/json");
    if (options.idempotencyKey !== undefined) headers.set("Idempotency-Key", options.idempotencyKey);
    const token = this.#token(credential);
    if (token !== null) headers.set("Authorization", `Bearer ${token}`);
    const method = (options.method ?? "GET").toUpperCase();
    const attempt = options.attempt ?? 0;
    const retryable = options.retry !== false
      && (["GET", "HEAD", "OPTIONS"].includes(method)
        || options.idempotencyKey !== undefined
        || options.replaySafe === true);
    let response: Response;
    try {
      response = await this.#fetch(`${this.baseUrl}${path}`, {
        method,
        headers,
        ...(options.body === undefined ? {} : { body: options.body }),
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
    } catch (cause) {
      if (!retryable || attempt + 1 >= MAX_REQUEST_ATTEMPTS || options.signal?.aborted === true) {
        throw cause;
      }
      await cancellableDelay(fallbackRetryDelay(attempt), options.signal);
      return this.request<T>(path, { ...options, attempt: attempt + 1 });
    }
    if (response.status === 401
      && options.retry !== false
      && options.authRetried !== true
      && (credential === "user" || credential === "either")
      && this.#session !== null) {
      await this.refreshSession(options.signal);
      return this.request<T>(path, { ...options, authRetried: true });
    }
    if (retryable
      && ([429, 503, 504].includes(response.status)
        || options.replaySafe === true && response.status >= 500)
      && attempt + 1 < MAX_REQUEST_ATTEMPTS) {
      await cancellableDelay(responseRetryDelay(response, attempt), options.signal);
      return this.request<T>(path, { ...options, attempt: attempt + 1 });
    }
    if (!response.ok) throw await responseError(response);
    const body = await readBoundedText(response, MAX_API_RESPONSE_BODY_BYTES, "internal.response_too_large");
    if (body.trim().length === 0) return undefined as T;
    try {
      return JSON.parse(body) as T;
    } catch {
      throw localError("internal.invalid_response", "FFDB returned an invalid success response", response.status);
    }
  }

  async workerRequest<T>(path: string, options: InternalRequestOptions = {}): Promise<T> {
    const response = await this.request<T | { readonly type: string; readonly payload: T }>(path, options);
    if (typeof response === "object" && response !== null && "type" in response && "payload" in response) {
      return response.payload;
    }
    return response;
  }

  providerFetch(input: string | URL | Request, init?: RequestInit): Promise<Response> {
    return this.#fetch(input, init);
  }

  async #loadSession(): Promise<void> {
    if (this.#loaded) return;
    this.#session = await this.#sessionStore.get();
    this.#loaded = true;
  }

  async #loadDeveloperSession(): Promise<void> {
    if (this.#developerLoaded) return;
    this.#developerSession = await this.#developerSessionStore.get();
    this.#developerLoaded = true;
  }

  #token(mode: CredentialMode): string | null {
    if (mode === "none") return null;
    if (mode === "platform") {
      if (this.#developerSession === null) throw localError("developer.session_missing", "Developer session required");
      return this.#developerSession.session_token;
    }
    if (mode === "developer") {
      if (this.#developerKey === null) throw localError("api_key.missing", "Developer key required");
      return this.#developerKey;
    }
    if (mode === "admin") {
      const token = this.#developerKey ?? this.#developerSession?.session_token;
      if (token === null || token === undefined) {
        throw localError("commerce.admin_credential_missing", "Developer key or platform session required");
      }
      return token;
    }
    if (mode === "user") {
      if (this.#session === null) throw localError("auth.session_missing", "User session required");
      return this.#session.access_token;
    }
    return this.#session?.access_token ?? this.#developerKey;
  }
}

function responseRetryDelay(response: Response, attempt: number): number {
  const value = response.headers.get("retry-after")?.trim();
  if (value !== undefined && value !== "") {
    const seconds = Number(value);
    if (Number.isFinite(seconds) && seconds >= 0) {
      return Math.min(MAX_RETRY_DELAY_MS, Math.round(seconds * 1_000));
    }
    const date = Date.parse(value);
    if (Number.isFinite(date)) {
      return Math.min(MAX_RETRY_DELAY_MS, Math.max(0, date - Date.now()));
    }
  }
  return fallbackRetryDelay(attempt);
}

function fallbackRetryDelay(attempt: number): number {
  const ceiling = Math.min(MAX_RETRY_DELAY_MS, 200 * (2 ** attempt));
  return Math.round(ceiling * (0.5 + Math.random() * 0.5));
}

function cancellableDelay(delayMs: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted === true) return Promise.reject(new DOMException("Aborted", "AbortError"));
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      signal?.removeEventListener("abort", abort);
      resolve();
    }, delayMs);
    const abort = () => {
      clearTimeout(timeout);
      reject(new DOMException("Aborted", "AbortError"));
    };
    signal?.addEventListener("abort", abort, { once: true });
  });
}

function pageQuery(page: { readonly limit?: number; readonly offset?: number }): string {
  const query = new URLSearchParams();
  if (page.limit !== undefined) {
    if (!Number.isSafeInteger(page.limit) || page.limit < 1 || page.limit > 500) {
      throw new TypeError("page limit must be an integer between 1 and 500");
    }
    query.set("limit", String(page.limit));
  }
  if (page.offset !== undefined) {
    if (!Number.isSafeInteger(page.offset) || page.offset < 0) {
      throw new TypeError("page offset must be a non-negative safe integer");
    }
    query.set("offset", String(page.offset));
  }
  return query.size === 0 ? "" : `?${query}`;
}

function newIdempotencyKey(prefix: string): string {
  const crypto = globalThis.crypto;
  if (typeof crypto?.randomUUID === "function") return `${prefix}:${crypto.randomUUID()}`;
  if (typeof crypto?.getRandomValues !== "function") {
    throw new TypeError("Secure randomness is required for mutation idempotency keys");
  }
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] ?? 0) & 0x0f | 0x40;
  bytes[8] = (bytes[8] ?? 0) & 0x3f | 0x80;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${prefix}:${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export class CommerceClient {
  constructor(private readonly client: FFDBClient) {}

  account(options: RequestOptions = {}): Promise<CommerceAccountSummary | null> {
    return this.client.request(this.client.projectPath("commerce/account"), {
      ...options,
      credential: "admin",
    });
  }

  disconnectAccount(options: RequestOptions = {}): Promise<void> {
    return this.client.request(this.client.projectPath("commerce/account"), {
      method: "DELETE",
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-disconnect"),
      credential: "admin",
    });
  }

  configureByo(
    input: ConfigureCommerceByoRequest,
    options: RequestOptions = {},
  ): Promise<CommerceAccountSummary> {
    return this.client.request(this.client.projectPath("commerce/account/byo"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-byo"),
      credential: "admin",
    });
  }

  connectOnboarding(
    input: CreateCommerceConnectOnboardingRequest,
    options: RequestOptions = {},
  ): Promise<CommerceOnboardingResponse> {
    return this.client.request(this.client.projectPath("commerce/account/connect/onboarding"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-connect"),
      credential: "admin",
    });
  }

  refreshAccount(options: RequestOptions = {}): Promise<CommerceAccountSummary> {
    return this.client.request(this.client.projectPath("commerce/account/refresh"), {
      method: "POST",
      ...options,
      credential: "admin",
    });
  }

  products(includeInactive = false, options: RequestOptions = {}): Promise<readonly CommerceProductSummary[]> {
    const suffix = includeInactive ? "?include_inactive=true" : "";
    return this.client.request(`${this.client.projectPath("commerce/products")}${suffix}`, {
      ...options,
      credential: includeInactive ? "admin" : "none",
    });
  }

  createProduct(
    input: CreateCommerceProductRequest,
    options: RequestOptions = {},
  ): Promise<CommerceProductSummary> {
    return this.client.request(this.client.projectPath("commerce/products"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-product"),
      credential: "admin",
    });
  }

  archiveProduct(productId: string, options: RequestOptions = {}): Promise<void> {
    return this.client.request(
      this.client.projectPath(`commerce/products/${encodeURIComponent(productId)}`),
      {
        method: "DELETE",
        ...options,
        idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-product-archive"),
        credential: "admin",
      },
    );
  }

  prices(includeInactive = false, options: RequestOptions = {}): Promise<readonly CommercePriceSummary[]> {
    const suffix = includeInactive ? "?include_inactive=true" : "";
    return this.client.request(`${this.client.projectPath("commerce/prices")}${suffix}`, {
      ...options,
      credential: includeInactive ? "admin" : "none",
    });
  }

  createPrice(
    input: CreateCommercePriceRequest,
    options: RequestOptions = {},
  ): Promise<CommercePriceSummary> {
    return this.client.request(this.client.projectPath("commerce/prices"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-price"),
      credential: "admin",
    });
  }

  retirePrice(priceId: string, options: RequestOptions = {}): Promise<void> {
    return this.client.request(
      this.client.projectPath(`commerce/prices/${encodeURIComponent(priceId)}`),
      {
        method: "DELETE",
        ...options,
        idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-price-retire"),
        credential: "admin",
      },
    );
  }

  oneTimeCheckout(
    input: CreateOneTimeCommerceCheckoutRequest,
    options: RequestOptions = {},
  ): Promise<CommerceCheckoutResponse> {
    return this.client.request(this.client.projectPath("commerce/checkouts/one-time"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-checkout"),
      credential: "either",
    });
  }

  recurringCheckout(
    input: CreateRecurringCommerceCheckoutRequest,
    options: RequestOptions = {},
  ): Promise<CommerceCheckoutResponse> {
    return this.client.request(this.client.projectPath("commerce/checkouts/recurring"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-subscription-checkout"),
      credential: "either",
    });
  }

  customerPortal(
    input: CreateCommerceCustomerPortalRequest,
    options: RequestOptions = {},
  ): Promise<BillingRedirect> {
    return this.client.request(this.client.projectPath("commerce/customer-portal"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-customer-portal"),
      credential: "either",
    });
  }

  orders(options: RequestOptions = {}): Promise<readonly CommerceOrderSummary[]> {
    return this.client.request(this.client.projectPath("commerce/orders"), {
      ...options,
      credential: "admin",
    });
  }

  order(orderId: string, options: RequestOptions = {}): Promise<CommerceOrderSummary> {
    return this.client.request(
      this.client.projectPath(`commerce/orders/${encodeURIComponent(orderId)}`),
      { ...options, credential: "admin" },
    );
  }

  updateFulfillment(
    orderId: string,
    status: CommerceFulfillmentStatus,
    note: string | null = null,
    options: RequestOptions = {},
  ): Promise<CommerceOrderSummary> {
    return this.client.request(
      this.client.projectPath(`commerce/orders/${encodeURIComponent(orderId)}/fulfillment`),
      {
        method: "PATCH",
        body: JSON.stringify({ status, note }),
        ...options,
        idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-fulfillment"),
        credential: "admin",
      },
    );
  }

  payments(options: RequestOptions = {}): Promise<readonly CommercePaymentSummary[]> {
    return this.client.request(this.client.projectPath("commerce/payments"), {
      ...options,
      credential: "admin",
    });
  }

  refund(
    input: CreateCommerceRefundRequest,
    options: RequestOptions = {},
  ): Promise<CommerceRefundSummary> {
    return this.client.request(this.client.projectPath("commerce/refunds"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-refund"),
      credential: "admin",
    });
  }

  subscriptions(options: RequestOptions = {}): Promise<readonly CommerceSubscriptionSummary[]> {
    return this.client.request(this.client.projectPath("commerce/subscriptions"), {
      ...options,
      credential: "admin",
    });
  }

  cancelSubscription(
    subscriptionId: string,
    input: CancelCommerceSubscriptionRequest,
    options: RequestOptions = {},
  ): Promise<CommerceSubscriptionSummary> {
    return this.client.request(
      this.client.projectPath(`commerce/subscriptions/${encodeURIComponent(subscriptionId)}/cancel`),
      {
        method: "POST",
        body: JSON.stringify(input),
        ...options,
        idempotencyKey: options.idempotencyKey ?? newIdempotencyKey("commerce-subscription-cancel"),
        credential: "admin",
      },
    );
  }

  entitlements(
    subject: { readonly kind: string; readonly id: string },
    atMs?: number,
    options: RequestOptions = {},
  ): Promise<readonly CommerceEntitlementSummary[]> {
    const query = new URLSearchParams({ subject_kind: subject.kind, subject_id: subject.id });
    if (atMs !== undefined) query.set("at_ms", String(atMs));
    return this.client.request(`${this.client.projectPath("commerce/entitlements")}?${query}`, {
      ...options,
      credential: "either",
    });
  }
}

export class AuthClient {
  constructor(private readonly client: FFDBClient) {}

  register(input: RegisterRequest, options: RequestOptions = {}): Promise<RegisterResponse> {
    return this.client.request(this.client.projectPath("auth/register"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      credential: "none",
    });
  }

  verifyEmail(token: string, options: RequestOptions = {}): Promise<void> {
    return this.client.request(this.client.projectPath("auth/verify"), {
      method: "POST",
      body: JSON.stringify({ token }),
      ...options,
      credential: "none",
    });
  }

  async signIn(email: string, password: string, options: RequestOptions = {}): Promise<AuthTokenPair> {
    const session = await this.client.request<AuthTokenPair>(this.client.projectPath("auth/sign-in"), {
      method: "POST",
      body: JSON.stringify({ email, password }),
      ...options,
      credential: "none",
    });
    await this.client.setSession(session);
    return session;
  }

  async signOut(options: RequestOptions = {}): Promise<void> {
    const session = await this.client.currentSession();
    if (session !== null) {
      await this.client.request(this.client.projectPath("auth/sign-out"), {
        method: "POST",
        body: JSON.stringify({ refresh_token: session.refresh_token }),
        ...options,
        credential: "user",
      });
    }
    await this.client.setSession(null);
  }

  session(): Promise<AuthTokenPair | null> {
    return this.client.currentSession();
  }

  refresh(signal?: AbortSignal): Promise<AuthTokenPair> {
    return this.client.refreshSession(signal);
  }

  startPasswordReset(email: string, options: RequestOptions = {}): Promise<void> {
    return this.client.request(this.client.projectPath("auth/password/reset"), {
      method: "POST",
      body: JSON.stringify({ email }),
      ...options,
      credential: "none",
    });
  }

  completePasswordReset(
    token: string,
    newPassword: string,
    options: RequestOptions = {},
  ): Promise<void> {
    return this.client.request(this.client.projectPath("auth/password/reset/complete"), {
      method: "POST",
      body: JSON.stringify({ token, new_password: newPassword }),
      ...options,
      credential: "none",
    });
  }

  sessions(options: RequestOptions = {}): Promise<readonly SessionSummary[]> {
    return this.client.request(this.client.projectPath("auth/sessions"), {
      ...options,
      credential: "user",
    });
  }

  revokeSession(id: string, options: RequestOptions = {}): Promise<void> {
    return this.client.request(this.client.projectPath(`auth/sessions/${encodeURIComponent(id)}`), {
      method: "DELETE",
      ...options,
      credential: "user",
    });
  }
}

export class StorageClient {
  constructor(private readonly client: FFDBClient) {}

  buckets(options: RequestOptions = {}): Promise<readonly StorageBucket[]> {
    return this.client.request(this.client.projectPath("storage/buckets"), {
      ...options,
      credential: "developer",
    });
  }

  createBucket(input: StorageBucketRequest, options: RequestOptions = {}): Promise<StorageBucket> {
    return this.client.request(this.client.projectPath("storage/buckets"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      credential: "developer",
    });
  }

  cleanupReservations(options: RequestOptions = {}): Promise<{
    readonly removed: number;
    readonly retried: number;
  }> {
    return this.client.request(this.client.projectPath("storage/cleanup"), {
      method: "POST",
      ...options,
      credential: "developer",
    });
  }

  sign(input: SignObjectRequest, options: RequestOptions = {}): Promise<SignedObjectRequest> {
    return this.client.request(this.client.projectPath("storage/sign"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      credential: "user",
    });
  }

  list(
    bucket: string,
    input: { readonly prefix?: string; readonly limit?: number; readonly cursor?: string } = {},
    options: RequestOptions = {},
  ): Promise<StorageObjectPage> {
    const query = new URLSearchParams({ bucket });
    if (input.prefix !== undefined) query.set("prefix", input.prefix);
    if (input.limit !== undefined) query.set("limit", String(input.limit));
    if (input.cursor !== undefined) query.set("cursor", input.cursor);
    return this.client.request(`${this.client.projectPath("storage/objects")}?${query}`, {
      ...options,
      credential: "user",
    });
  }

  async downloadUrl(
    bucket: string,
    key: string,
    options: RequestOptions = {},
  ): Promise<SignedObjectRequest> {
    return this.sign({
      bucket,
      key,
      operation: "download",
      content_type: null,
      size_bytes: null,
      checksum_sha256: null,
    }, options);
  }

  async upload(
    bucket: string,
    key: string,
    body: BodyInit,
    metadata: { readonly sizeBytes: number; readonly contentType: string; readonly checksumSha256?: string },
    options: RequestOptions = {},
  ): Promise<void> {
    const signed = await this.sign(
      {
        bucket,
        key,
        operation: "upload",
        content_type: metadata.contentType,
        size_bytes: metadata.sizeBytes,
        checksum_sha256: metadata.checksumSha256 ?? null,
      },
      options,
    );
    const authorizationToken = requiredAuthorizationToken(signed);
    let providerSucceeded = false;
    try {
      const response = await this.client.providerFetch(signed.url, {
        method: signed.method,
        headers: providerHeaders(signed),
        body,
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
      if (!response.ok) throw localError("storage.upload_failed", "Object provider rejected upload", response.status);
      providerSucceeded = true;
      await this.commit(authorizationToken, options);
    } catch (error) {
      if (!providerSucceeded) await this.releaseBestEffort(authorizationToken, options);
      throw error;
    }
  }

  async createMultipart(
    bucket: string,
    key: string,
    metadata: {
      readonly sizeBytes: number;
      readonly contentType?: string;
      readonly checksumSha256?: string;
    },
    options: RequestOptions = {},
  ): Promise<MultipartUpload> {
    const authorization = await this.client.request<{ readonly authorization_token: string }>(
      this.client.projectPath("storage/multipart/authorize"),
      {
        method: "POST",
        body: JSON.stringify({
          bucket,
          key,
          content_type: metadata.contentType ?? null,
          size_bytes: metadata.sizeBytes,
          checksum_sha256: metadata.checksumSha256 ?? null,
        }),
        ...options,
        credential: "user",
      },
    );
    const created = await this.client.request<{ readonly upload_id: string }>(
      this.client.projectPath("storage/multipart/create"),
      {
        method: "POST",
        body: JSON.stringify({ authorization_token: authorization.authorization_token }),
        ...options,
        credential: "user",
        replaySafe: true,
      },
    );
    return { bucket, key, uploadId: created.upload_id };
  }

  async uploadPart(
    upload: MultipartUpload,
    partNumber: number,
    body: BodyInit,
    metadata: {
      readonly sizeBytes: number;
      readonly contentType?: string;
      readonly checksumSha256?: string;
    },
    options: RequestOptions = {},
  ): Promise<MultipartPart> {
    validatePartNumber(partNumber);
    const signed = await this.sign(
      {
        bucket: upload.bucket,
        key: upload.key,
        operation: "upload_part",
        content_type: metadata.contentType ?? null,
        size_bytes: metadata.sizeBytes,
        checksum_sha256: metadata.checksumSha256 ?? null,
        upload_id: upload.uploadId,
        part_number: partNumber,
      },
      options,
    );
    const authorizationToken = requiredAuthorizationToken(signed);
    let providerSucceeded = false;
    try {
      const response = await this.client.providerFetch(signed.url, {
        method: signed.method,
        headers: providerHeaders(signed),
        body,
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
      if (!response.ok) {
        throw localError(
          "storage.multipart_part_failed",
          "Object provider rejected multipart upload part",
          response.status,
        );
      }
      providerSucceeded = true;
      const etag = requiredMultipartEtag(response.headers.get("etag"));
      await this.commitMultipart(authorizationToken, "upload_part", { etag }, options);
      return { partNumber, etag };
    } catch (error) {
      if (!providerSucceeded) await this.releaseBestEffort(authorizationToken, options);
      throw error;
    }
  }

  async completeMultipart(
    upload: MultipartUpload,
    parts: readonly MultipartPart[],
    metadata: {
      readonly sizeBytes: number;
      readonly contentType?: string;
      readonly checksumSha256?: string;
    },
    options: RequestOptions = {},
  ): Promise<void> {
    const orderedParts = validateAndOrderParts(parts);
    const signed = await this.sign(
      {
        bucket: upload.bucket,
        key: upload.key,
        operation: "complete_multipart",
        content_type: metadata.contentType ?? null,
        size_bytes: metadata.sizeBytes,
        checksum_sha256: metadata.checksumSha256 ?? null,
        upload_id: upload.uploadId,
        part_number: null,
      },
      options,
    );
    const authorizationToken = requiredAuthorizationToken(signed);
    let providerSucceeded = false;
    try {
      const response = await this.client.providerFetch(signed.url, {
        method: signed.method,
        headers: providerHeaders(signed),
        body: multipartCompletionXml(orderedParts),
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
      const providerBody = await readBoundedProviderText(response);
      providerSucceeded = response.ok;
      if (!response.ok || /<(?:[A-Za-z0-9_-]+:)?Error(?:\s|>)/u.test(providerBody)) {
        throw localError(
          "storage.multipart_complete_failed",
          "Object provider rejected multipart upload completion",
          response.status,
        );
      }
      await this.commitMultipart(authorizationToken, "complete", {}, options);
    } catch (error) {
      if (!providerSucceeded) await this.releaseBestEffort(authorizationToken, options);
      throw error;
    }
  }

  async abortMultipart(
    upload: MultipartUpload,
    options: RequestOptions = {},
  ): Promise<void> {
    const signed = await this.sign(
      {
        bucket: upload.bucket,
        key: upload.key,
        operation: "abort_multipart",
        content_type: null,
        size_bytes: null,
        checksum_sha256: null,
        upload_id: upload.uploadId,
        part_number: null,
      },
      options,
    );
    const authorizationToken = requiredAuthorizationToken(signed);
    let providerSucceeded = false;
    try {
      const response = await this.client.providerFetch(signed.url, {
        method: signed.method,
        headers: providerHeaders(signed),
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
      if (!response.ok) {
        throw localError(
          "storage.multipart_abort_failed",
          "Object provider rejected multipart upload abort",
          response.status,
        );
      }
      providerSucceeded = true;
      await this.commitMultipart(authorizationToken, "abort", {}, options);
    } catch (error) {
      if (!providerSucceeded) await this.releaseBestEffort(authorizationToken, options);
      throw error;
    }
  }

  async delete(bucket: string, key: string, options: RequestOptions = {}): Promise<void> {
    const signed = await this.sign({
      bucket,
      key,
      operation: "delete",
      content_type: null,
      size_bytes: null,
      checksum_sha256: null,
    }, options);
    const authorizationToken = requiredAuthorizationToken(signed);
    let providerSucceeded = false;
    try {
      const response = await this.client.providerFetch(signed.url, {
        method: signed.method,
        headers: providerHeaders(signed),
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
      if (!response.ok) throw localError("storage.delete_failed", "Object provider rejected delete", response.status);
      providerSucceeded = true;
      await this.commit(authorizationToken, options);
    } catch (error) {
      if (!providerSucceeded) await this.releaseBestEffort(authorizationToken, options);
      throw error;
    }
  }

  private async commit(authorizationToken: string, options: RequestOptions): Promise<void> {
    await this.client.request(this.client.projectPath("storage/commit"), {
      method: "POST",
      body: JSON.stringify({ authorization_token: authorizationToken }),
      ...options,
      credential: "user",
      replaySafe: true,
    });
  }

  private async commitMultipart(
    authorizationToken: string,
    operation: "upload_part" | "complete" | "abort",
    result: { readonly upload_id?: string; readonly etag?: string },
    options: RequestOptions,
  ): Promise<void> {
    await this.client.request(this.client.projectPath("storage/multipart/commit"), {
      method: "POST",
      body: JSON.stringify({
        authorization_token: authorizationToken,
        operation,
        upload_id: result.upload_id ?? null,
        etag: result.etag ?? null,
      }),
      ...options,
      credential: "user",
      replaySafe: true,
    });
  }

  private async releaseBestEffort(authorizationToken: string, options: RequestOptions): Promise<void> {
    const { signal: _abortedProviderSignal, ...releaseOptions } = options;
    try {
      await this.client.request(this.client.projectPath("storage/release"), {
        method: "POST",
        body: JSON.stringify({ authorization_token: authorizationToken }),
        ...releaseOptions,
        credential: "user",
      });
    } catch {
      // The durable reservation expires and is cleaned by trusted maintenance.
    }
  }
}

const MAX_API_RESPONSE_BODY_BYTES = 9 * 1024 * 1024;
const MAX_MULTIPART_PROVIDER_BODY_BYTES = 128 * 1024;

async function readBoundedText(response: Response, maximumBytes: number, errorCode: string): Promise<string> {
  const declaredLength = Number(response.headers.get("content-length"));
  if (Number.isFinite(declaredLength) && declaredLength > maximumBytes) {
    throw localError(errorCode, "Response body is too large");
  }
  if (response.body === null) return "";
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > maximumBytes) {
      await reader.cancel();
      throw localError(errorCode, "Response body is too large");
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

async function readBoundedProviderText(response: Response): Promise<string> {
  return readBoundedText(
    response,
    MAX_MULTIPART_PROVIDER_BODY_BYTES,
    "storage.provider_response_too_large",
  );
}

function requiredMultipartEtag(value: string | null): string {
  const etag = value?.trim() ?? "";
  if (etag.length === 0 || etag.length > 256 || /[\u0000-\u001f\u007f]/u.test(etag)) {
    throw localError("storage.multipart_etag_missing", "Object provider omitted a valid part ETag");
  }
  return etag;
}

function validatePartNumber(partNumber: number): void {
  if (!Number.isInteger(partNumber) || partNumber < 1 || partNumber > 10_000) {
    throw localError("storage.multipart_part_invalid", "Multipart part number must be an integer from 1 to 10000");
  }
}

function validateAndOrderParts(parts: readonly MultipartPart[]): readonly MultipartPart[] {
  if (parts.length === 0 || parts.length > 10_000) {
    throw localError("storage.multipart_parts_invalid", "Multipart completion requires 1 to 10000 parts");
  }
  const ordered = [...parts].sort((left, right) => left.partNumber - right.partNumber);
  let previous = 0;
  for (const part of ordered) {
    validatePartNumber(part.partNumber);
    if (part.partNumber === previous) {
      throw localError("storage.multipart_parts_invalid", "Multipart part numbers must be unique");
    }
    requiredMultipartEtag(part.etag);
    previous = part.partNumber;
  }
  return ordered;
}

function multipartCompletionXml(parts: readonly MultipartPart[]): string {
  const body = parts.map((part) =>
    `<Part><PartNumber>${part.partNumber}</PartNumber><ETag>${escapeXml(part.etag)}</ETag></Part>`
  ).join("");
  return `<CompleteMultipartUpload>${body}</CompleteMultipartUpload>`;
}

function escapeXml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

function requiredAuthorizationToken(signed: SignedObjectRequest): string {
  if (signed.authorization_token === null) {
    throw localError("storage.authorization_missing", "Storage authorization token is missing");
  }
  return signed.authorization_token;
}

function providerHeaders(signed: SignedObjectRequest): Headers {
  return new Headers(
    signed.headers
      // Browsers calculate this forbidden header from BodyInit. It remains part
      // of SigV4 SignedHeaders and the provider verifies the calculated value.
      .filter(([name]) => name.toLowerCase() !== "content-length")
      .map(([name, value]) => [name, value]),
  );
}

export class SyncClient {
  constructor(private readonly client: FFDBClient) {}

  pull(cursor: string | null, limit = 1_000, options: RequestOptions = {}): Promise<SyncPullResponse> {
    const query = new URLSearchParams({ limit: String(limit) });
    if (cursor !== null) query.set("cursor", cursor);
    return this.client.workerRequest(`${this.client.projectPath("sync")}?${query}`, {
      ...options,
      credential: "user",
    });
  }

  push(input: SyncPushRequest, options: RequestOptions = {}): Promise<SyncPushResponse> {
    return this.client.workerRequest(this.client.projectPath("sync/push"), {
      method: "POST",
      body: JSON.stringify(input),
      ...options,
      credential: "user",
    });
  }

  snapshot(tables?: readonly string[], options: RequestOptions = {}): Promise<SnapshotResponse> {
    const query = new URLSearchParams();
    for (const table of tables ?? []) query.append("table", table);
    const suffix = query.size === 0 ? "" : `?${query}`;
    return this.client.workerRequest(`${this.client.projectPath("snapshot")}${suffix}`, {
      ...options,
      credential: "user",
    });
  }
}

async function responseError(response: Response): Promise<FFDBError> {
  try {
    const payload = (await response.json()) as ErrorEnvelope;
    if (payload.error?.code && payload.error.message) return new FFDBError(response.status, payload.error);
  } catch {
    // A provider-controlled response is deliberately collapsed to a safe error.
  }
  return localError("internal.invalid_response", "FFDB returned an invalid error response", response.status);
}

function localError(code: string, message: string, status = 0): FFDBError {
  return new FFDBError(status, { code, message, request_id: "" });
}
