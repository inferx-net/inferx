\set ON_ERROR_STOP on

-- Idempotency: prevent duplicate credits for the same payment reference.
-- Partial unique index: rows without a payment_ref (e.g., manual admin credits
-- without a ref) are exempt.

-- Step 1: Check for existing duplicates. If any exist, null out payment_ref
-- on older rows (keeps the newest row with the ref, nulls the rest).
UPDATE TenantCreditHistory
SET payment_ref = NULL
WHERE payment_ref IS NOT NULL
  AND id NOT IN (
    SELECT MAX(id) FROM TenantCreditHistory
    WHERE payment_ref IS NOT NULL
    GROUP BY tenant, payment_ref
  );

-- Step 2: Create the unique index.
CREATE UNIQUE INDEX IF NOT EXISTS idx_tenant_credit_payment_ref_uniq
    ON TenantCreditHistory (tenant, payment_ref)
    WHERE payment_ref IS NOT NULL;
