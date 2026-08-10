# FFDB HTTP API reference

Generated from docs/API/openapi.json. The deployed /openapi.json document remains authoritative for machine-readable schemas.

## System

- GET /healthz — health; auth: public; arguments: none; body: none; returns: 200: object; errors: none declared
- GET /readyz — readiness; auth: public; arguments: none; body: none; returns: 200: object; 503: ErrorEnvelope; errors: 503
- GET /metrics — metrics; auth: public; arguments: none; body: none; returns: 200: string; 503: ErrorEnvelope; errors: 503
- GET /openapi.json — openapi; auth: public; arguments: none; body: none; returns: 200: object; errors: none declared

## Instance

- GET /v1/instance/setup/status — getPublicInstanceSetupStatus; auth: public; arguments: none; body: none; returns: 200: object; 503: ErrorEnvelope; errors: 503
- GET /v1/instance — getInstance; auth: developerBearer; arguments: none; body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/instance — completeOrReconfigureInstance; auth: developerBearer; arguments: Idempotency-Key (header, required, string); body: required object JSON; returns: 200: object; 400: ErrorEnvelope; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 400, 403, 409; Idempotency-Key required
- GET /v1/instance/updates — getHostUpdateStatus; auth: developerBearer; arguments: none; body: none; returns: 200: HostUpdateStatus; 403: ErrorEnvelope; 503: ErrorEnvelope; errors: 403, 503
- POST /v1/instance/updates/check — checkForHostUpdate; auth: developerBearer; arguments: none; body: none; returns: 202: HostUpdateJob; 403: ErrorEnvelope; 409: ErrorEnvelope; 503: ErrorEnvelope; errors: 403, 409, 503
- POST /v1/instance/updates/install — installHostUpdate; auth: developerBearer; arguments: none; body: required HostUpdateVersionRequest JSON; returns: 202: HostUpdateJob; 400: ErrorEnvelope; 403: ErrorEnvelope; 409: ErrorEnvelope; 428: ErrorEnvelope; 503: ErrorEnvelope; errors: 400, 403, 409, 428, 503
- POST /v1/instance/updates/rollback — rollbackHostUpdate; auth: developerBearer; arguments: none; body: required HostUpdateVersionRequest JSON; returns: 202: HostUpdateJob; 400: ErrorEnvelope; 403: ErrorEnvelope; 409: ErrorEnvelope; 428: ErrorEnvelope; 503: ErrorEnvelope; errors: 400, 403, 409, 428, 503
- GET /v1/instance/updates/jobs/{job_id} — getHostUpdateJob; auth: developerBearer; arguments: job_id (path, required, string); body: none; returns: 200: HostUpdateJob; 400: ErrorEnvelope; 403: ErrorEnvelope; 404: ErrorEnvelope; 503: ErrorEnvelope; errors: 400, 403, 404, 503
- GET /v1/instance/updates/settings — getHostUpdateSettings; auth: developerBearer; arguments: none; body: none; returns: 200: HostUpdateSettings; 403: ErrorEnvelope; 503: ErrorEnvelope; errors: 403, 503
- PATCH /v1/instance/updates/settings — configureHostUpdates; auth: developerBearer; arguments: none; body: required HostUpdateSettings JSON; returns: 202: HostUpdateJob; 400: ErrorEnvelope; 403: ErrorEnvelope; 428: ErrorEnvelope; 503: ErrorEnvelope; errors: 400, 403, 428, 503
- PATCH /v1/instance/organization-creation-policy — updateOrganizationCreationPolicy; auth: developerBearer; arguments: none; body: required object JSON; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/instance/billing/connect/onboarding — createInstanceConnectOnboarding; auth: developerBearer; arguments: Idempotency-Key (header, required, string); body: required object JSON; returns: 200: object; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409; Idempotency-Key required
- POST /v1/instance/billing/refresh — refreshInstanceBillingAccount; auth: developerBearer; arguments: none; body: none; returns: 200: object; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409
- GET /v1/instance/administrators — listInstanceAdministrators; auth: developerBearer; arguments: none; body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/instance/administrators — grantInstanceAdministrator; auth: developerBearer; arguments: none; body: required object JSON; returns: 200: object; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409
- DELETE /v1/instance/administrators/{user_id} — revokeInstanceAdministrator; auth: developerBearer; arguments: user_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409
- GET /v1/instance/organizations — listInstanceOrganizations; auth: developerBearer; arguments: none; body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- PATCH /v1/instance/organizations/{organization_id} — setInstanceOrganizationDisabled; auth: developerBearer; arguments: organization_id (path, required, string); body: required { disabled: boolean } JSON; returns: 200: object; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409
- GET /v1/instance/users — listInstanceUsers; auth: developerBearer; arguments: none; body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- PATCH /v1/instance/users/{user_id} — setInstanceUserDisabled; auth: developerBearer; arguments: user_id (path, required, string); body: required { disabled: boolean } JSON; returns: 200: object; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409
- GET /v1/instance/billing-exemptions — listInstanceBillingExemptions; auth: developerBearer; arguments: none; body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- PUT /v1/instance/billing-exemptions/{organization_id} — grantInstanceBillingExemption; auth: developerBearer; arguments: organization_id (path, required, string); body: required object JSON; returns: 200: object; 403: ErrorEnvelope; errors: 403
- DELETE /v1/instance/billing-exemptions/{organization_id} — revokeInstanceBillingExemption; auth: developerBearer; arguments: organization_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- GET /v1/instance/plans — listInstancePlans; auth: developerBearer; arguments: none; body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- PUT /v1/instance/plans/{tier} — putInstancePlan; auth: developerBearer; arguments: tier (path, required, "free" | "pay_as_you_go" | "pro"); body: required object JSON; returns: 200: object; 400: ErrorEnvelope; 403: ErrorEnvelope; errors: 400, 403
- DELETE /v1/instance/plans/{tier} — retireInstancePlan; auth: developerBearer; arguments: tier (path, required, "free" | "pay_as_you_go" | "pro"); body: none; returns: 200: object; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409

## Developer auth

- POST /v1/developer/bootstrap — bootstrapDeveloper; auth: public; arguments: none; body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409
- POST /v1/developer/sign-in — developerSignIn; auth: public; arguments: none; body: required object JSON; returns: 200: object; 401: ErrorEnvelope; errors: 401
- POST /v1/developer/refresh — developerRefresh; auth: public; arguments: none; body: required object JSON; returns: 200: object; 401: ErrorEnvelope; errors: 401
- POST /v1/developer/sign-out — developerSignOut; auth: public; arguments: none; body: required object JSON; returns: 200: object; 401: ErrorEnvelope; errors: 401
- POST /v1/developer/invitations/accept — acceptOrganizationInvitation; auth: public; arguments: none; body: required object JSON; returns: 200: object; 400: ErrorEnvelope; errors: 400

## Organizations

- GET /v1/organizations — listOrganizations; auth: developerBearer; arguments: none; body: none; returns: 200: object; 401: ErrorEnvelope; errors: 401
- POST /v1/organizations — createOrganization; auth: developerBearer; arguments: none; body: required object JSON; returns: 200: object; 400: ErrorEnvelope; errors: 400
- GET /v1/organizations/{organization_id}/projects — listProjects; auth: developerBearer; arguments: organization_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- GET /v1/organizations/{organization_id}/members — listMembers; auth: developerBearer; arguments: organization_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/organizations/{organization_id}/members — addMember; auth: developerBearer; arguments: organization_id (path, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409
- PATCH /v1/organizations/{organization_id}/members/{user_id} — updateMember; auth: developerBearer; arguments: organization_id (path, required, string); user_id (path, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409
- DELETE /v1/organizations/{organization_id}/members/{user_id} — removeMember; auth: developerBearer; arguments: organization_id (path, required, string); user_id (path, required, string); body: none; returns: 200: object; 409: ErrorEnvelope; errors: 409
- POST /v1/organizations/{organization_id}/invitations — inviteMember; auth: developerBearer; arguments: organization_id (path, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409

## Projects

- POST /v1/projects — createProject; auth: developerBearer; arguments: Idempotency-Key (header, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409; Idempotency-Key required
- GET /v1/projects/{project_id}/api-keys — listApiKeys; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/api-keys — createApiKey; auth: developerBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/api-keys/{api_key_id}/revoke — revokeApiKey; auth: developerBearer; arguments: project_id (path, required, string); api_key_id (path, required, string); body: none; returns: 200: object; 404: ErrorEnvelope; errors: 404
- POST /v1/projects/{project_id}/keys/rotate — rotateSigningKey; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403

## End-user auth

- POST /v1/projects/{project_id}/auth/register — registerUser; auth: public; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 400: ErrorEnvelope; errors: 400
- POST /v1/projects/{project_id}/auth/verify — verifyEmail; auth: public; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 400: ErrorEnvelope; errors: 400
- POST /v1/projects/{project_id}/auth/sign-in — userSignIn; auth: public; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 401: ErrorEnvelope; errors: 401
- POST /v1/projects/{project_id}/auth/refresh — userRefresh; auth: public; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 401: ErrorEnvelope; errors: 401
- POST /v1/projects/{project_id}/auth/sign-out — userSignOut; auth: public; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 401: ErrorEnvelope; errors: 401
- POST /v1/projects/{project_id}/auth/password/reset — startPasswordReset; auth: public; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; errors: none declared
- POST /v1/projects/{project_id}/auth/password/reset/complete — completePasswordReset; auth: public; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 400: ErrorEnvelope; errors: 400
- POST /v1/projects/{project_id}/auth/password/change — changePassword; auth: userBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 401: ErrorEnvelope; errors: 401
- GET /v1/projects/{project_id}/auth/sessions — listSessions; auth: userBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 401: ErrorEnvelope; errors: 401
- DELETE /v1/projects/{project_id}/auth/sessions/{session_id} — revokeSession; auth: userBearer; arguments: project_id (path, required, string); session_id (path, required, string); body: none; returns: 200: object; 404: ErrorEnvelope; errors: 404
- GET /v1/projects/{project_id}/auth/settings — getAuthSettings; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- PATCH /v1/projects/{project_id}/auth/settings — updateAuthSettings; auth: developerBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 400: ErrorEnvelope; errors: 400
- GET /v1/projects/{project_id}/auth/users — listAuthUsers; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- PATCH /v1/projects/{project_id}/auth/users/{user_id} — updateAuthUser; auth: developerBearer; arguments: project_id (path, required, string); user_id (path, required, string); body: required object JSON; returns: 200: object; 404: ErrorEnvelope; errors: 404

## Data

- POST /v1/projects/{project_id}/query — query; auth: projectBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/transaction — transaction; auth: projectBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409
- GET /v1/projects/{project_id}/schema — schema; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- GET /v1/projects/{project_id}/policies — policies; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- GET /v1/projects/{project_id}/migrations — migrationHistory; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/migrations — applyMigration; auth: developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409; Idempotency-Key required
- POST /v1/projects/{project_id}/migrations/{migration_id}/rollback — rollbackMigration; auth: developerBearer; arguments: project_id (path, required, string); migration_id (path, required, string); Idempotency-Key (header, required, string); body: none; returns: 200: object; 409: ErrorEnvelope; errors: 409; Idempotency-Key required
- POST /v1/projects/{project_id}/seed — seed; auth: developerBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 403: ErrorEnvelope; errors: 403

## Billing

- GET /v1/organizations/{organization_id}/billing — getOrganizationBilling; auth: developerBearer; arguments: organization_id (path, required, string); body: none; returns: 200: PlatformBillingSummary; 403: ErrorEnvelope; 503: ErrorEnvelope; errors: 403, 503
- POST /v1/organizations/{organization_id}/billing/checkout — createOrganizationBillingCheckout; auth: developerBearer; arguments: organization_id (path, required, string); Idempotency-Key (header, required, string); body: required CreatePlatformCheckoutRequest JSON; returns: 201: BillingRedirect; 400: ErrorEnvelope; 403: ErrorEnvelope; 409: ErrorEnvelope; 503: ErrorEnvelope; errors: 400, 403, 409, 503; Idempotency-Key required
- POST /v1/organizations/{organization_id}/billing/portal — createOrganizationBillingPortal; auth: developerBearer; arguments: organization_id (path, required, string); Idempotency-Key (header, required, string); body: none; returns: 201: BillingRedirect; 403: ErrorEnvelope; 409: ErrorEnvelope; 503: ErrorEnvelope; errors: 403, 409, 503; Idempotency-Key required
- GET /v1/organizations/{organization_id}/billing/invoices — listOrganizationBillingInvoices; auth: developerBearer; arguments: organization_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- GET /v1/organizations/{organization_id}/billing/usage — getOrganizationBillingUsage; auth: developerBearer; arguments: organization_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/billing/webhooks/stripe — receiveStripeBillingWebhook; auth: public; arguments: Stripe-Signature (header, required, string); body: required object JSON; returns: 200: { received: boolean; duplicate?: boolean }; 400: ErrorEnvelope; 409: ErrorEnvelope; 503: ErrorEnvelope; errors: 400, 409, 503

## Commerce

- GET /v1/projects/{project_id}/payments — getProjectPaymentsSummary; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: ProjectPaymentsSummary; 403: ErrorEnvelope; errors: 403
- GET /v1/projects/{project_id}/commerce/account — getCommerceAccount; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: CommerceAccountSummary; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409
- DELETE /v1/projects/{project_id}/commerce/account — disconnectCommerceAccount; auth: developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: none; returns: 204: Local commerce account binding removed; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409; Idempotency-Key required
- POST /v1/projects/{project_id}/commerce/account/byo — configureCommerceByo; auth: developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: required ConfigureCommerceByoRequest JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409; Idempotency-Key required
- POST /v1/projects/{project_id}/commerce/account/connect/onboarding — createCommerceConnectOnboarding; auth: developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409; Idempotency-Key required
- POST /v1/projects/{project_id}/commerce/account/refresh — refreshCommerceAccount; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; errors: none declared
- GET /v1/projects/{project_id}/commerce/products — listCommerceProducts; auth: public; arguments: project_id (path, required, string); body: none; returns: 200: object; errors: none declared
- POST /v1/projects/{project_id}/commerce/products — createCommerceProduct; auth: developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: required object JSON; returns: 201: object; errors: none declared; Idempotency-Key required
- DELETE /v1/projects/{project_id}/commerce/products/{product_id} — archiveCommerceProduct; auth: developerBearer; arguments: project_id (path, required, string); product_id (path, required, string); Idempotency-Key (header, required, string); body: none; returns: 204: Product archived; errors: none declared; Idempotency-Key required
- GET /v1/projects/{project_id}/commerce/prices — listCommercePrices; auth: public; arguments: project_id (path, required, string); body: none; returns: 200: object; errors: none declared
- POST /v1/projects/{project_id}/commerce/prices — createCommercePrice; auth: developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: required object JSON; returns: 201: object; errors: none declared; Idempotency-Key required
- DELETE /v1/projects/{project_id}/commerce/prices/{price_id} — retireCommercePrice; auth: developerBearer; arguments: project_id (path, required, string); price_id (path, required, string); Idempotency-Key (header, required, string); body: none; returns: 204: Price retired; errors: none declared; Idempotency-Key required
- POST /v1/projects/{project_id}/commerce/checkouts/one-time — createOneTimeCommerceCheckout; auth: projectBearer or developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: required object JSON; returns: 201: object; errors: none declared; Idempotency-Key required
- POST /v1/projects/{project_id}/commerce/checkouts/recurring — createRecurringCommerceCheckout; auth: projectBearer or developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: required object JSON; returns: 201: object; errors: none declared; Idempotency-Key required
- POST /v1/projects/{project_id}/commerce/customer-portal — createCommerceCustomerPortal; auth: projectBearer or developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: required { subject: { kind: "individual" | "team" | "organization"; id: string }; return_url: string } JSON; returns: 201: BillingRedirect; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409; Idempotency-Key required
- GET /v1/projects/{project_id}/commerce/orders — listCommerceOrders; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; errors: none declared
- GET /v1/projects/{project_id}/commerce/orders/{order_id} — getCommerceOrder; auth: developerBearer; arguments: project_id (path, required, string); order_id (path, required, string); body: none; returns: 200: object; errors: none declared
- PATCH /v1/projects/{project_id}/commerce/orders/{order_id}/fulfillment — updateCommerceFulfillment; auth: developerBearer; arguments: project_id (path, required, string); order_id (path, required, string); Idempotency-Key (header, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409; Idempotency-Key required
- GET /v1/projects/{project_id}/commerce/payments — listCommercePayments; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; errors: none declared
- POST /v1/projects/{project_id}/commerce/refunds — createCommerceRefund; auth: developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: required object JSON; returns: 201: object; 409: ErrorEnvelope; errors: 409; Idempotency-Key required
- GET /v1/projects/{project_id}/commerce/subscriptions — listCommerceSubscriptions; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; errors: none declared
- POST /v1/projects/{project_id}/commerce/subscriptions/{subscription_id}/cancel — cancelCommerceSubscription; auth: developerBearer; arguments: project_id (path, required, string); subscription_id (path, required, string); Idempotency-Key (header, required, string); body: required object JSON; returns: 200: object; errors: none declared; Idempotency-Key required
- GET /v1/projects/{project_id}/commerce/entitlements — listCommerceEntitlements; auth: projectBearer or developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; errors: none declared
- POST /v1/projects/{project_id}/commerce/webhooks/stripe — receiveProjectCommerceByoStripeWebhook; auth: public; arguments: project_id (path, required, string); Stripe-Signature (header, required, string); body: required object JSON; returns: 200: Verified BYO event processed or deduplicated; 400: ErrorEnvelope; 409: ErrorEnvelope; errors: 400, 409
- POST /v1/commerce/webhooks/stripe-connect — receiveProjectCommerceConnectStripeWebhook; auth: public; arguments: Stripe-Signature (header, required, string); body: required object JSON; returns: 200: Verified account-routed Connect event processed or deduplicated; 400: ErrorEnvelope; 409: ErrorEnvelope; 503: ErrorEnvelope; errors: 400, 409, 503

## Sync

- GET /v1/projects/{project_id}/snapshot — snapshot; auth: userBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 409: ErrorEnvelope; errors: 409
- GET /v1/projects/{project_id}/sync — syncPull; auth: userBearer; arguments: project_id (path, required, string); cursor (query, optional, string); limit (query, optional, integer); body: none; returns: 200: object; 409: ErrorEnvelope; errors: 409
- POST /v1/projects/{project_id}/sync/push — syncPush; auth: userBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409

## Storage

- GET /v1/projects/{project_id}/storage/buckets — listBuckets; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/storage/buckets — createBucket; auth: developerBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409
- POST /v1/projects/{project_id}/storage/sign — signStorageOperation; auth: userBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/storage/commit — commitStorageOperation; auth: userBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409
- POST /v1/projects/{project_id}/storage/release — releaseStorageOperation; auth: userBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409
- GET /v1/projects/{project_id}/storage/objects — listObjects; auth: userBearer; arguments: project_id (path, required, string); bucket (query, required, string); prefix (query, optional, string); cursor (query, optional, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/storage/cleanup — cleanupStorageReservations; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/storage/multipart/authorize — authorizeMultipartCreate; auth: userBearer; arguments: project_id (path, required, string); body: required { bucket: string; key: string; content_type?: string | null; size_bytes: integer; checksum_sha256?: string | null } JSON; returns: 200: { authorization_token: string }; 403: ErrorEnvelope; 409: ErrorEnvelope; errors: 403, 409
- POST /v1/projects/{project_id}/storage/multipart/create — createMultipartUpload; auth: userBearer; arguments: project_id (path, required, string); body: required { authorization_token: string } JSON; returns: 201: { upload_id: string }; 403: ErrorEnvelope; 409: ErrorEnvelope; 503: ErrorEnvelope; errors: 403, 409, 503
- POST /v1/projects/{project_id}/storage/multipart/commit — commitMultipartStage; auth: userBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 409: ErrorEnvelope; errors: 409

## Email

- GET /v1/projects/{project_id}/email/templates — listEmailTemplates; auth: developerBearer; arguments: project_id (path, required, string); kind (query, optional, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/email/templates/artifacts — importEmailArtifact; auth: developerBearer; arguments: project_id (path, required, string); body: required object JSON; returns: 200: object; 422: ErrorEnvelope; errors: 422
- POST /v1/projects/{project_id}/email/templates/{kind}/{version}/publish — publishEmailTemplate; auth: developerBearer; arguments: project_id (path, required, string); kind (path, required, "verification" | "password_reset" | "email_change" | "invitation" | "magic_link"); version (path, required, integer); body: none; returns: 200: object; 404: ErrorEnvelope; errors: 404
- POST /v1/projects/{project_id}/email/templates/{kind}/{version}/preview — previewEmailTemplate; auth: developerBearer; arguments: project_id (path, required, string); kind (path, required, "verification" | "password_reset" | "email_change" | "invitation" | "magic_link"); version (path, required, integer); body: required object JSON; returns: 200: object; 404: ErrorEnvelope; errors: 404

## Operations

- GET /v1/instance/observability — getInstanceObservability; auth: developerBearer; arguments: range (query, optional, "1h" | "6h" | "24h" | "7d" | "30d"); project_id (query, optional, string); body: none; returns: 200: ObservabilitySummary; 400: ErrorEnvelope; 403: ErrorEnvelope; 503: ErrorEnvelope; errors: 400, 403, 503
- GET /v1/projects/{project_id}/backups — listBackups; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- POST /v1/projects/{project_id}/backups — createBackup; auth: developerBearer; arguments: project_id (path, required, string); Idempotency-Key (header, required, string); body: none; returns: 200: object; 409: ErrorEnvelope; errors: 409; Idempotency-Key required
- POST /v1/projects/{project_id}/backups/{backup_id}/restore — restoreBackup; auth: developerBearer; arguments: project_id (path, required, string); backup_id (path, required, string); Idempotency-Key (header, required, string); body: none; returns: 200: WorkerRestoreResponse; 409: ErrorEnvelope; errors: 409; Idempotency-Key required
- GET /v1/projects/{project_id}/logs — auditLogs; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 403: ErrorEnvelope; errors: 403
- GET /v1/projects/{project_id}/observability — getProjectObservability; auth: developerBearer; arguments: project_id (path, required, string); range (query, optional, "1h" | "6h" | "24h" | "7d" | "30d"); body: none; returns: 200: ObservabilitySummary; 400: ErrorEnvelope; 403: ErrorEnvelope; 503: ErrorEnvelope; errors: 400, 403, 503
- POST /v1/projects/{project_id}/integrity-check — integrityCheck; auth: developerBearer; arguments: project_id (path, required, string); body: none; returns: 200: object; 503: ErrorEnvelope; errors: 503
