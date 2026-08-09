-- Reverse lookup used by organization discovery, instance user summaries, and
-- the platform_users foreign key. The memberships primary key has the opposite
-- column order and cannot serve user-only predicates.
CREATE INDEX organization_memberships_user_idx
    ON organization_memberships (user_id, organization_id);

-- Audit reads and append-chain verification order by the monotonic append
-- sequence. The older occurred_at index cannot satisfy that ordering.
CREATE INDEX audit_events_project_sequence_idx
    ON audit_events (project_id, append_sequence DESC)
    WHERE project_id IS NOT NULL;
CREATE INDEX audit_events_organization_sequence_idx
    ON audit_events (organization_id, append_sequence DESC)
    WHERE project_id IS NULL AND organization_id IS NOT NULL;

CREATE INDEX backups_project_created_idx
    ON backups (project_id, created_at DESC)
    WHERE state <> 'deleted';

CREATE INDEX auth_sessions_user_created_idx
    ON auth_sessions (project_id, user_id, created_at DESC);

-- Commerce collections are project-scoped. Keep their common list order and
-- relationship checks index-backed as tenant data grows.
CREATE INDEX commerce_products_project_created_idx
    ON commerce_products (project_id, created_at, id);
CREATE INDEX commerce_prices_project_created_idx
    ON commerce_prices (project_id, created_at, id);
CREATE INDEX commerce_order_lines_order_idx
    ON commerce_order_lines (project_id, order_id, id);
CREATE INDEX commerce_payments_project_created_idx
    ON commerce_payments (project_id, created_at DESC, id);
CREATE INDEX commerce_payments_order_status_idx
    ON commerce_payments (project_id, order_id, status)
    WHERE order_id IS NOT NULL;
CREATE INDEX commerce_refunds_payment_status_idx
    ON commerce_refunds (project_id, payment_id, status);
CREATE INDEX commerce_subscriptions_project_created_idx
    ON commerce_subscriptions (project_id, created_at DESC, id);
CREATE INDEX commerce_entitlements_subscription_active_idx
    ON commerce_entitlements (project_id, subscription_id)
    WHERE subscription_id IS NOT NULL AND status = 'active';
CREATE INDEX commerce_fulfillment_events_order_idx
    ON commerce_fulfillment_events (project_id, order_id, created_at);
