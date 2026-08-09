export type JsonScalar = string | number | boolean | null;
export type JsonValue = JsonScalar | JsonValue[] | { readonly [key: string]: JsonValue };

export type SqlParameter =
  | { readonly type: "null" }
  | { readonly type: "integer"; readonly value: number | string }
  | { readonly type: "real"; readonly value: number }
  | { readonly type: "text"; readonly value: string }
  | { readonly type: "blob"; readonly value: string };

export interface QueryOptions {
  readonly max_rows?: number;
}

export interface QueryRequest {
  readonly sql: string;
  readonly parameters?: readonly SqlParameter[];
  readonly options?: QueryOptions;
}

export type DeclaredColumnType =
  | "null"
  | "integer"
  | "real"
  | "text"
  | "blob"
  | "date"
  | "timestamp"
  | "unknown";

export interface ColumnMetadata {
  readonly name: string;
  readonly type: DeclaredColumnType;
  readonly origin_table?: string;
}

export interface BlobValue {
  readonly $blob: string;
}

export type ResultCell = null | number | string | BlobValue;

export interface QueryResult<Row extends readonly ResultCell[] = readonly ResultCell[]> {
  readonly columns: readonly ColumnMetadata[];
  readonly rows: readonly Row[];
  readonly affected_rows: number;
  readonly last_insert_rowid: number | null;
  readonly truncated: boolean;
}

export interface TransactionRequest {
  readonly statements: readonly QueryRequest[];
}

export interface PlatformErrorBody {
  readonly code: string;
  readonly message: string;
  readonly request_id: string;
  readonly details?: Readonly<Record<string, JsonValue>>;
}

export interface ErrorEnvelope {
  readonly error: PlatformErrorBody;
}

export interface AuthUser {
  readonly id: string;
  readonly email: string;
  readonly email_verified: boolean;
  readonly disabled: boolean;
  readonly role: string;
  readonly custom_claims: Readonly<Record<string, JsonValue>>;
  readonly created_at_ms: number;
}

export interface SetAuthUserDisabledRequest {
  readonly disabled: boolean;
}

export interface AuthTokenPair {
  readonly access_token: string;
  readonly refresh_token: string;
  readonly token_type: string;
  readonly expires_in_seconds: number;
  readonly session_id: string;
  readonly user: AuthUser;
}

export interface DeveloperSession {
  readonly session_token: string;
  readonly user_id: string;
  readonly email: string;
  readonly expires_at_ms: number;
}

export interface RegisterRequest {
  readonly email: string;
  readonly password: string;
  readonly custom_claims?: Readonly<Record<string, JsonValue>>;
}

export interface RegisterResponse {
  readonly user_id: string;
  readonly verification_required: boolean;
}

export interface SessionSummary {
  readonly id: string;
  readonly created_at_ms: number;
  readonly last_seen_at_ms: number;
  readonly expires_at_ms: number;
  readonly user_agent: string | null;
  readonly ip_address: string | null;
  readonly current: boolean;
  readonly revoked_at_ms: number | null;
}

export type OrganizationRole = "owner" | "admin" | "developer" | "viewer";

export interface OrganizationSummary {
  readonly id: string;
  readonly name: string;
  readonly slug: string;
  readonly role: OrganizationRole;
  readonly created_at_ms: number;
}

export interface OrganizationMembershipSummary {
  readonly organization_id: string;
  readonly user_id: string;
  readonly email: string;
  readonly role: OrganizationRole;
  readonly created_at_ms: number;
}

export interface AddOrganizationMemberRequest {
  readonly email: string;
  readonly role: OrganizationRole;
}

export interface UpdateOrganizationMemberRequest {
  readonly role: OrganizationRole;
}

export interface CreateOrganizationInvitationRequest {
  readonly email: string;
  readonly role: OrganizationRole;
}

export interface AcceptOrganizationInvitationRequest {
  readonly invitation_token: string;
  readonly password: string;
}

export type ProjectLifecycleState =
  | "provisioning"
  | "active"
  | "suspended"
  | "restoring"
  | "deleting"
  | "deleted"
  | "failed";

export interface ProjectSummary {
  readonly id: string;
  readonly organization_id: string;
  readonly name: string;
  readonly slug: string;
  readonly region: string;
  readonly state: ProjectLifecycleState;
  readonly schema_version: number;
  readonly created_at_ms: number;
}

export type PlatformBillingTier = "free" | "pay_as_you_go" | "pro";

export type PlatformBillingStatus =
  | "free"
  | "checkout_pending"
  | "trialing"
  | "active"
  | "past_due"
  | "unpaid"
  | "canceled"
  | "paused"
  | "incomplete";

export type PlatformBillingUnit = "organization" | "seat";

export interface PlatformUsageAllowance {
  readonly storage_bytes: number;
  readonly monthly_reads: number;
  readonly monthly_writes: number;
  readonly monthly_active_users: number;
  readonly overage_enabled: boolean;
}

export type UsageReportingStatus = "healthy" | "degraded" | "reconciling" | "blocked";

export interface PlatformUsageSummary {
  readonly organization_id: string;
  readonly period_start_ms: number;
  readonly period_end_ms: number;
  readonly reads: number;
  readonly writes: number;
  readonly storage_bytes: number;
  readonly storage_byte_hours: number;
  readonly monthly_active_users: number;
  readonly reporting_status: UsageReportingStatus;
  readonly reporting_last_success_ms: number | null;
  readonly as_of_ms: number;
}

export interface PlatformBillingSummary {
  readonly organization_id: string;
  readonly tier: PlatformBillingTier;
  readonly status: PlatformBillingStatus;
  readonly billing_unit: PlatformBillingUnit;
  readonly seat_quantity: number;
  readonly project_limit: number | null;
  readonly usage_allowance: PlatformUsageAllowance;
  readonly current_period_start_ms: number | null;
  readonly current_period_end_ms: number | null;
  readonly cancel_at_period_end: boolean;
  readonly provider_configured: boolean;
  readonly billing_enforcement_enabled: boolean;
  readonly billing_exempt: boolean;
}

export interface CreatePlatformCheckoutRequest {
  readonly tier: PlatformBillingTier;
}

export interface BillingRedirect {
  readonly url: string;
}

export type InstanceDeploymentMode =
  | "unconfigured"
  | "private"
  | "team"
  | "platform_byo"
  | "platform_connect";
export type OrganizationCreationPolicy = "owner_only" | "authenticated" | "invitation_only";
export type InstanceAdministratorRole = "owner" | "admin";
export type InstanceBillingMode = "byo_keys" | "stripe_connect";
export type InstanceBillingAccountStatus =
  | "pending"
  | "onboarding"
  | "enabled"
  | "restricted"
  | "disconnected";

export interface PublicInstanceSetupStatus {
  readonly bootstrap_available: boolean;
  readonly setup_required: boolean;
  readonly platform_byo_available: boolean;
  readonly platform_connect_available: boolean;
}

export interface InstanceBillingAccountSummary {
  readonly mode: InstanceBillingMode;
  readonly status: InstanceBillingAccountStatus;
  readonly provider_account_id: string | null;
  readonly charges_enabled: boolean;
  readonly payouts_enabled: boolean;
  readonly details_submitted: boolean;
  readonly capabilities: readonly string[];
  readonly credentials_configured: boolean;
  readonly updated_at_ms: number;
}

export interface InstanceStatus {
  readonly owner_user_id: string;
  readonly current_user_role: InstanceAdministratorRole;
  readonly deployment_mode: InstanceDeploymentMode;
  readonly organization_creation_policy: OrganizationCreationPolicy;
  readonly billing_enforcement_enabled: boolean;
  readonly setup_completed_at_ms: number | null;
  readonly billing_account: InstanceBillingAccountSummary | null;
  readonly administrator_count: number;
  readonly created_at_ms: number;
  readonly updated_at_ms: number;
}

export type CompleteInstanceSetupRequest =
  | {
      readonly deployment_mode: "private";
      readonly organization_creation_policy: OrganizationCreationPolicy;
    }
  | {
      readonly deployment_mode: "team";
      readonly organization_creation_policy: OrganizationCreationPolicy;
    }
  | {
      readonly deployment_mode: "platform_byo";
      readonly organization_creation_policy: OrganizationCreationPolicy;
      readonly secret_key: string;
      readonly webhook_secret: string;
    }
  | {
      readonly deployment_mode: "platform_connect";
      readonly organization_creation_policy: OrganizationCreationPolicy;
      readonly secret_key: string;
      readonly webhook_secret: string;
      readonly country: string;
      readonly email: string;
      readonly return_url: string;
      readonly refresh_url: string;
    };

export interface InstanceBillingOnboarding {
  readonly url: string;
  readonly expires_at_ms: number;
}

export interface CompleteInstanceSetupResponse {
  readonly instance: InstanceStatus;
  readonly onboarding: InstanceBillingOnboarding | null;
}

export interface CreateInstanceConnectOnboardingRequest {
  readonly return_url: string;
  readonly refresh_url: string;
}

export interface UpdateOrganizationCreationPolicyRequest {
  readonly organization_creation_policy: OrganizationCreationPolicy;
}

export interface InstanceAdministratorSummary {
  readonly user_id: string;
  readonly email: string;
  readonly role: InstanceAdministratorRole;
  readonly granted_by: string | null;
  readonly created_at_ms: number;
}

export interface GrantInstanceAdministratorRequest {
  readonly user_id: string;
}

export interface InstanceOrganizationSummary {
  readonly id: string;
  readonly name: string;
  readonly slug: string;
  readonly disabled: boolean;
  readonly member_count: number;
  readonly project_count: number;
  readonly billing_exempt: boolean;
  readonly created_at_ms: number;
}

export interface InstanceOrganizationPage {
  readonly organizations: readonly InstanceOrganizationSummary[];
  readonly total: number;
  readonly limit: number;
  readonly offset: number;
}

export interface InstanceUserSummary {
  readonly id: string;
  readonly email: string;
  readonly email_verified: boolean;
  readonly disabled: boolean;
  readonly instance_role: InstanceAdministratorRole | null;
  readonly organization_count: number;
  readonly created_at_ms: number;
}

export interface InstanceUserPage {
  readonly users: readonly InstanceUserSummary[];
  readonly total: number;
  readonly limit: number;
  readonly offset: number;
}

export interface UpdateInstanceDisabledRequest {
  readonly disabled: boolean;
}

export interface OrganizationBillingExemptionSummary {
  readonly organization_id: string;
  readonly organization_name: string;
  readonly reason: string;
  readonly created_by: string;
  readonly created_by_email: string;
  readonly created_at_ms: number;
}

export interface GrantOrganizationBillingExemptionRequest {
  readonly reason: string;
}

export type InstanceReadsAtLimit = "continue" | "overage";
export type InstanceWritesAtLimit = "pause" | "overage";
export type InstanceSignupsAtLimit = "pause" | "overage";

export interface InstancePlanCatalogEntry {
  readonly tier: PlatformBillingTier;
  readonly display_name: string;
  readonly billing_unit: PlatformBillingUnit;
  readonly base_price_cents: number | null;
  readonly currency: string;
  readonly project_limit: number | null;
  readonly storage_bytes: number;
  readonly monthly_reads: number;
  readonly monthly_writes: number;
  readonly monthly_active_users: number;
  readonly overage_enabled: boolean;
  readonly reads_at_limit: InstanceReadsAtLimit;
  readonly writes_at_limit: InstanceWritesAtLimit;
  readonly signups_at_limit: InstanceSignupsAtLimit;
  readonly requires_payment_method_for_overage: boolean;
  readonly active: boolean;
  /** Provider-priced fields cannot change while this Stripe catalog is active. */
  readonly provider_catalog_bound: boolean;
  readonly updated_at_ms: number;
}

export type PutInstancePlanCatalogEntryRequest = Omit<
  InstancePlanCatalogEntry,
  "tier" | "provider_catalog_bound" | "updated_at_ms"
>;

export type PlatformInvoiceStatus =
  | "draft"
  | "open"
  | "paid"
  | "uncollectible"
  | "void"
  | "payment_failed";

export interface PlatformInvoiceSummary {
  readonly id: string;
  readonly organization_id: string;
  readonly status: PlatformInvoiceStatus;
  readonly currency: string;
  readonly amount_due_minor: number;
  readonly amount_paid_minor: number;
  readonly period_start_ms: number | null;
  readonly period_end_ms: number | null;
  readonly hosted_invoice_url: string | null;
  readonly invoice_pdf_url: string | null;
  readonly created_at_ms: number;
}

/**
 * Compatibility summary for the project-level payments capability endpoint.
 * Detailed commerce administration is available through `client.commerce`.
 */
export type ProjectPaymentsStatus = "not_configured" | "enabled" | "restricted";

export interface ProjectPaymentCapabilities {
  readonly checkout_sessions: boolean;
  readonly recurring_billing: boolean;
  readonly customer_portal: boolean;
  readonly webhooks: boolean;
}

export interface ProjectPaymentsSummary {
  readonly project_id: string;
  readonly organization_id: string;
  readonly status: ProjectPaymentsStatus;
  readonly provider: string | null;
  readonly capabilities: ProjectPaymentCapabilities;
}

export type CommerceProviderMode = "bring_your_own_keys" | "stripe_connect";
export type CommerceAccountStatus = "configuring" | "onboarding" | "enabled" | "restricted" | "disconnected";

export interface CommerceAccountCapabilities {
  readonly one_time_payments: boolean;
  readonly recurring_payments: boolean;
  readonly refunds: boolean;
  readonly customer_portal: boolean;
}

export interface CommerceAccountSummary {
  readonly project_id: string;
  readonly mode: CommerceProviderMode;
  readonly status: CommerceAccountStatus;
  readonly livemode: boolean;
  readonly provider_account_id: string | null;
  readonly capabilities: CommerceAccountCapabilities;
  readonly requirements_due: readonly string[];
  readonly disabled_reason: string | null;
  readonly webhook_url: string;
  readonly secrets_configured: boolean;
}

export interface ConfigureCommerceByoRequest {
  readonly secret_key: string;
  readonly webhook_secret: string;
}

export interface CreateCommerceConnectOnboardingRequest {
  readonly country: string;
  readonly email: string;
  readonly return_url: string;
  readonly refresh_url: string;
}

export interface CommerceOnboardingResponse {
  readonly account: CommerceAccountSummary;
  readonly onboarding_url: string;
  readonly expires_at_ms: number;
}

export type CommerceProductStatus = "draft" | "active" | "archived";

export interface CreateCommerceProductRequest {
  readonly name: string;
  readonly description: string | null;
  readonly tax_code: string | null;
  readonly metadata?: Readonly<Record<string, JsonValue>>;
}

export interface CommerceProductSummary extends CreateCommerceProductRequest {
  readonly id: string;
  readonly project_id: string;
  readonly status: CommerceProductStatus;
  readonly metadata: Readonly<Record<string, JsonValue>>;
  readonly created_at_ms: number;
  readonly updated_at_ms: number;
}

export type CommerceBillingIntervalUnit = "day" | "week" | "month" | "year";
export type CommerceEntitlementValue =
  | { readonly type: "enabled"; readonly value: boolean }
  | { readonly type: "quantity"; readonly value: number }
  | { readonly type: "text"; readonly value: string };
export type CommercePriceBilling =
  | { readonly type: "one_time" }
  | { readonly type: "recurring"; readonly interval: CommerceBillingIntervalUnit; readonly interval_count: number };

export interface CreateCommercePriceRequest {
  readonly product_id: string;
  readonly lookup_key: string | null;
  readonly currency: string;
  readonly unit_amount_minor: number;
  readonly billing: CommercePriceBilling;
  readonly entitlements?: Readonly<Record<string, CommerceEntitlementValue>>;
}

export interface CommercePriceSummary extends CreateCommercePriceRequest {
  readonly id: string;
  readonly project_id: string;
  readonly entitlements: Readonly<Record<string, CommerceEntitlementValue>>;
  readonly active: boolean;
  readonly created_at_ms: number;
}

export type CommerceMembershipSubjectKind = "individual" | "team" | "organization";
export interface CommerceMembershipSubject {
  readonly kind: CommerceMembershipSubjectKind;
  readonly id: string;
}

export interface CommerceCheckoutLine {
  readonly price_id: string;
  readonly quantity: number;
}

export interface CreateOneTimeCommerceCheckoutRequest {
  readonly lines: readonly CommerceCheckoutLine[];
  readonly subject: CommerceMembershipSubject | null;
  readonly customer_email: string | null;
  readonly client_reference: string | null;
  readonly success_url: string;
  readonly cancel_url: string;
}

export interface CreateRecurringCommerceCheckoutRequest {
  readonly price_id: string;
  readonly quantity: number;
  readonly subject: CommerceMembershipSubject;
  readonly customer_email: string | null;
  readonly success_url: string;
  readonly cancel_url: string;
}

export interface CreateCommerceCustomerPortalRequest {
  readonly subject: CommerceMembershipSubject;
  readonly return_url: string;
}

export interface CommerceCheckoutResponse {
  readonly url: string;
  readonly expires_at_ms: number;
  readonly order_id: string | null;
  readonly subscription_id: string | null;
}

export type CommerceOrderStatus = "pending" | "checkout_created" | "processing" | "paid" | "payment_failed" | "canceled" | "partially_refunded" | "refunded";
export type CommerceFulfillmentStatus = "unfulfilled" | "processing" | "fulfilled" | "canceled";

export interface CommerceOrderLineSummary {
  readonly product_id: string;
  readonly price_id: string;
  readonly product_name: string;
  readonly currency: string;
  readonly unit_amount_minor: number;
  readonly quantity: number;
  readonly line_total_minor: number;
}

export interface CommerceOrderSummary {
  readonly id: string;
  readonly project_id: string;
  readonly customer_id: string | null;
  readonly client_reference: string | null;
  readonly status: CommerceOrderStatus;
  readonly fulfillment_status: CommerceFulfillmentStatus;
  readonly currency: string;
  readonly subtotal_minor: number;
  readonly discount_minor: number;
  readonly tax_minor: number;
  readonly shipping_minor: number;
  readonly total_minor: number;
  readonly refunded_minor: number;
  readonly lines: readonly CommerceOrderLineSummary[];
  readonly paid_at_ms: number | null;
  readonly created_at_ms: number;
  readonly updated_at_ms: number;
}

export type CommercePaymentStatus = "requires_payment_method" | "requires_action" | "processing" | "authorized" | "captured" | "partially_refunded" | "refunded" | "failed" | "canceled";
export interface CommercePaymentSummary {
  readonly id: string;
  readonly project_id: string;
  readonly order_id: string | null;
  readonly subscription_id: string | null;
  readonly status: CommercePaymentStatus;
  readonly currency: string;
  readonly authorized_minor: number;
  readonly captured_minor: number;
  readonly refunded_minor: number;
  readonly provider_created_at_ms: number;
  readonly created_at_ms: number;
}

export type CommerceRefundReason = "duplicate" | "fraudulent" | "requested_by_customer" | "other";
export interface CreateCommerceRefundRequest {
  readonly payment_id: string;
  readonly amount_minor: number | null;
  readonly reason: CommerceRefundReason | null;
}
export type CommerceRefundStatus = "pending" | "succeeded" | "failed" | "canceled";
export interface CommerceRefundSummary {
  readonly id: string;
  readonly payment_id: string;
  readonly status: CommerceRefundStatus;
  readonly amount_minor: number;
  readonly currency: string;
  readonly reason: CommerceRefundReason | null;
  readonly created_at_ms: number;
  readonly updated_at_ms: number;
}

export type CommerceSubscriptionStatus = "checkout_pending" | "trialing" | "active" | "past_due" | "unpaid" | "paused" | "canceled" | "incomplete" | "expired";
export interface CommerceSubscriptionSummary {
  readonly id: string;
  readonly project_id: string;
  readonly customer_id: string;
  readonly price_id: string;
  readonly subject: CommerceMembershipSubject;
  readonly quantity: number;
  readonly status: CommerceSubscriptionStatus;
  readonly current_period_start_ms: number | null;
  readonly current_period_end_ms: number | null;
  readonly cancel_at_period_end: boolean;
  readonly created_at_ms: number;
  readonly updated_at_ms: number;
}

export interface CancelCommerceSubscriptionRequest {
  readonly at_period_end: boolean;
}

export interface CommerceEntitlementSummary {
  readonly subject: CommerceMembershipSubject;
  readonly key: string;
  readonly value: CommerceEntitlementValue;
  readonly subscription_id: string | null;
  readonly order_id: string | null;
  readonly valid_from_ms: number;
  readonly valid_until_ms: number | null;
}

export type DeveloperScope =
  | "projects_read"
  | "projects_write"
  | "database_query"
  | "database_migrate"
  | "database_schema"
  | "auth_manage"
  | "storage_manage"
  | "email_manage"
  | "commerce_manage"
  | "keys_rotate"
  | "backups_manage"
  | "logs_read";

export interface CreatedApiKey {
  readonly id: string;
  readonly name: string;
  readonly prefix: string;
  readonly secret: string;
  readonly scopes: readonly DeveloperScope[];
  readonly expires_at_ms: number | null;
  readonly created_at_ms: number;
}

export interface ApiKeySummary {
  readonly id: string;
  readonly name: string;
  readonly prefix: string;
  readonly scopes: readonly DeveloperScope[];
  readonly expires_at_ms: number | null;
  readonly created_at_ms: number;
  readonly revoked_at_ms: number | null;
}

export interface MigrationSpec {
  readonly id: string;
  readonly name: string;
  readonly up_sql: string;
  readonly down_sql: string;
  readonly checksum: string;
  readonly created_at_ms: number;
}

export type PolicyCommand = "all" | "select" | "insert" | "update" | "delete";
export type PolicyKind = "permissive" | "restrictive";

export interface PolicyDefinition {
  readonly name: string;
  readonly table: string;
  readonly kind: PolicyKind;
  readonly command: PolicyCommand;
  readonly roles: readonly string[];
  readonly using_expression: string | null;
  readonly check_expression: string | null;
  readonly enabled: boolean;
  readonly forced: boolean;
}

export interface TableDefinition {
  readonly name: string;
  readonly sql: string;
  readonly rls_enabled: boolean;
  readonly rls_forced: boolean;
}

export interface SchemaSnapshot {
  readonly version: number;
  readonly tables: readonly TableDefinition[];
}

export type ChangeOperation = "insert" | "update" | "delete";

export interface LogicalChange {
  readonly sequence: number;
  readonly transaction_id: string;
  readonly table: string;
  readonly primary_key: JsonValue;
  readonly operation: ChangeOperation;
  readonly row_version: number;
  readonly values: Readonly<Record<string, JsonValue>> | null;
  readonly tombstone: JsonValue | null;
  readonly actor: string | null;
  readonly schema_version: number;
  readonly committed_at_ms: number;
  readonly client_mutation_id: string | null;
}

export type SyncControl =
  | {
      readonly type: "resnapshot_required";
      readonly reason: string;
      readonly minimum_schema_version: number;
    }
  | { readonly type: "invalidate_scope"; readonly scope_fingerprint: string };

export interface SyncPullResponse {
  readonly changes: readonly LogicalChange[];
  readonly cursor: string;
  readonly has_more: boolean;
  readonly control: SyncControl | null;
}

export interface SyncMutation {
  readonly mutation_id: string;
  readonly table: string;
  readonly primary_key: JsonValue;
  readonly operation: ChangeOperation;
  readonly values: Readonly<Record<string, JsonValue>> | null;
  readonly base_row_version: number | null;
  readonly client_timestamp_ms: number | null;
}

export interface SyncPushRequest {
  readonly schema_version: number;
  readonly mutations: readonly SyncMutation[];
}

export type MutationStatus = "applied" | "duplicate" | "rejected" | "superseded";

export interface SyncMutationResult {
  readonly mutation_id: string;
  readonly status: MutationStatus;
  readonly server_sequence: number | null;
  readonly row_version: number | null;
  readonly error_code: string | null;
}

export interface SyncPushResponse {
  readonly results: readonly SyncMutationResult[];
  readonly cursor: string;
}

export interface SnapshotResponse {
  readonly schema_version: number;
  readonly cursor: string;
  readonly tables: Readonly<Record<string, QueryResult>>;
}

export interface StorageBucketRequest {
  readonly name: string;
  readonly public: boolean;
  readonly max_object_bytes: number | null;
  readonly versioning: boolean;
}

export interface StorageBucket {
  readonly id: string;
  readonly name: string;
  readonly public: boolean;
  readonly max_object_bytes: number;
  readonly project_quota_bytes: number;
  readonly versioning: boolean;
  readonly created_at_ms: number;
}

export type ObjectOperation =
  | "upload"
  | "download"
  | "delete"
  | "upload_part"
  | "complete_multipart"
  | "abort_multipart";

export interface SignObjectRequest {
  readonly bucket: string;
  readonly key: string;
  readonly operation: ObjectOperation;
  readonly content_type: string | null;
  readonly size_bytes: number | null;
  readonly checksum_sha256: string | null;
  readonly upload_id?: string | null;
  readonly part_number?: number | null;
}

export interface SignedObjectRequest {
  readonly url: string;
  readonly method: string;
  readonly headers: readonly (readonly [string, string])[];
  readonly expires_at_ms: number;
  readonly authorization_token: string | null;
}

export interface StorageObjectItem {
  readonly id: string;
  readonly object_key: string;
  readonly owner_id: string;
  readonly size_bytes: number;
  readonly content_type: string | null;
  readonly checksum_sha256: string | null;
  readonly etag: string | null;
  readonly version_id: string | null;
  readonly created_at_ms: number;
  readonly updated_at_ms: number;
}

export interface StorageObjectPage {
  readonly items: readonly StorageObjectItem[];
  readonly next_cursor: string | null;
}

export interface MultipartUpload {
  readonly bucket: string;
  readonly key: string;
  readonly uploadId: string;
}

export interface MultipartPart {
  readonly partNumber: number;
  readonly etag: string;
}

export type EmailTemplateKind =
  | "verification"
  | "password_reset"
  | "email_change"
  | "invitation"
  | "magic_link";

export interface EmailTemplateVersion {
  readonly kind: EmailTemplateKind;
  readonly version: number;
  readonly source: string;
  readonly source_sha256: string;
  readonly subject_template: string;
  readonly html_template: string;
  readonly text_template: string;
  readonly allowed_variables: readonly string[];
  readonly artifact_status: "validated" | "rejected";
  readonly compilation_errors: readonly string[];
  readonly compiled_at_ms: number;
  readonly published_at_ms: number | null;
}

export interface EmailTemplateArtifactInput {
  readonly kind: EmailTemplateKind;
  readonly version: number;
  readonly source: string;
  readonly source_sha256: string;
  readonly subject_template: string;
  readonly html_template: string;
  readonly text_template: string;
  readonly allowed_variables: readonly string[];
}

export interface EmailTemplatePreview {
  readonly subject: string;
  readonly html: string;
  readonly text: string;
}

export type BackupStatus =
  | "queued"
  | "running"
  | "complete"
  | "failed"
  | "restoring"
  | "restore_verified";

export interface BackupSummary {
  readonly id: string;
  readonly project_id: string;
  readonly status: BackupStatus;
  readonly size_bytes: number | null;
  readonly sha256: string | null;
  readonly created_at_ms: number;
  readonly completed_at_ms: number | null;
  readonly last_restore_test_ms: number | null;
}

export interface BackupResult {
  readonly backup_id: string;
  readonly size_bytes: number;
  readonly sha256: string;
}

export interface RestoreResult {
  readonly backup_id: string;
  readonly integrity_ok: boolean;
  readonly schema_version: number;
}

export interface MigrationSummary {
  readonly id: string;
  readonly name: string;
  readonly checksum: string;
  readonly status: string;
  readonly schema_version_before: number;
  readonly schema_version_after: number;
  readonly applied_at_ms: number | null;
}

export interface AuthSettings {
  readonly registration_enabled: boolean;
  readonly email_verification_required: boolean;
  readonly access_token_ttl_seconds: number;
  readonly refresh_token_ttl_seconds: number;
  readonly password_min_length: number;
}

export interface AuditLogEntry {
  readonly id: string;
  readonly occurred_at_ms: number;
  readonly actor: string;
  readonly action: string;
  readonly resource: string;
  readonly outcome: "success" | "denied" | "failed";
  readonly request_id: string | null;
}

export type ObservabilityRange = "1h" | "6h" | "24h" | "7d" | "30d";

export interface ObservabilityHttpTotals {
  readonly requests: number;
  readonly qps: number;
  readonly client_errors: number;
  readonly server_errors: number;
  readonly error_rate: number;
  readonly average_latency_ms: number | null;
  readonly p50_latency_ms: number | null;
  readonly p95_latency_ms: number | null;
  readonly p99_latency_ms: number | null;
  readonly max_latency_ms: number | null;
}

export interface ObservabilityTimePoint {
  readonly timestamp_ms: number;
  readonly requests: number;
  readonly qps: number;
  readonly client_errors: number;
  readonly server_errors: number;
  readonly p50_latency_ms: number | null;
  readonly p95_latency_ms: number | null;
  readonly p99_latency_ms: number | null;
}

export interface ObservabilityRouteMetric {
  readonly method: string;
  readonly route: string;
  readonly requests: number;
  readonly qps: number;
  readonly error_rate: number;
  readonly average_latency_ms: number | null;
  readonly p50_latency_ms: number | null;
  readonly p95_latency_ms: number | null;
  readonly p99_latency_ms: number | null;
  readonly max_latency_ms: number | null;
}

export interface ObservabilityQueryMetric {
  readonly fingerprint: string;
  readonly shape: string;
  readonly statement_kind: string;
  readonly read_only: boolean;
  readonly executions: number;
  readonly errors: number;
  readonly error_rate: number;
  readonly average_latency_ms: number | null;
  readonly p50_latency_ms: number | null;
  readonly p95_latency_ms: number | null;
  readonly p99_latency_ms: number | null;
  readonly max_latency_ms: number | null;
  readonly rows_returned: number;
  readonly rows_affected: number;
}

export interface ObservabilityRuntimeSnapshot {
  readonly healthy: boolean;
  readonly active_workers: number;
  readonly max_workers: number;
  readonly worker_saturation: number;
  readonly execution_slots_in_use: number;
  readonly queue_capacity: number;
  readonly queue_saturation: number;
}

export interface ObservabilityStorageSnapshot {
  readonly logical_database_bytes: number;
  readonly sampled_projects: number;
  readonly database_disk_total_bytes: number | null;
  readonly database_disk_available_bytes: number | null;
  readonly database_disk_used_percent: number | null;
  readonly backup_disk_total_bytes: number | null;
  readonly backup_disk_available_bytes: number | null;
  readonly backup_disk_used_percent: number | null;
  readonly last_sample_at_ms: number | null;
}

export interface ObservabilitySummary {
  readonly scope: "instance" | "project";
  readonly project_id: string | null;
  readonly generated_at_ms: number;
  readonly window_start_ms: number;
  readonly window_end_ms: number;
  readonly resolution_seconds: number;
  readonly retention_days: number;
  readonly current_inflight: number;
  readonly dropped_samples: number;
  readonly totals: ObservabilityHttpTotals;
  readonly series: readonly ObservabilityTimePoint[];
  readonly busiest_routes: readonly ObservabilityRouteMetric[];
  readonly slowest_routes: readonly ObservabilityRouteMetric[];
  readonly frequent_queries: readonly ObservabilityQueryMetric[];
  readonly slow_queries: readonly ObservabilityQueryMetric[];
  readonly runtime: ObservabilityRuntimeSnapshot;
  readonly storage: ObservabilityStorageSnapshot;
}

export interface HealthStatus {
  readonly status: string;
  readonly version?: number;
}

export interface RequestOptions {
  readonly signal?: AbortSignal;
  readonly idempotencyKey?: string;
  readonly retry?: boolean;
}
