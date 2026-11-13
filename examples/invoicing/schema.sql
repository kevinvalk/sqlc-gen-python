-- The acceptance fixture for the generated Python: a small invoicing domain
-- that exercises every rule in docs/python-output.md.

CREATE SCHEMA billing;

CREATE TYPE invoice_status AS ENUM ('draft', 'issued', 'paid', 'void');

COMMENT ON TYPE invoice_status IS 'Lifecycle of an invoice. Only drafts may be deleted.';

-- Outside the default schema, and one label is not a Python identifier.
CREATE TYPE billing.payment_method AS ENUM ('bank_transfer', 'card', 'direct-debit');

CREATE TABLE customers (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    email text,
    tags text[] NOT NULL DEFAULT '{}',
    preferred_method billing.payment_method,
    accepted_methods billing.payment_method[] NOT NULL DEFAULT '{}',
    metadata jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE customers IS 'Someone we send invoices to.';

CREATE TABLE invoices (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id uuid NOT NULL REFERENCES customers (id),
    number text NOT NULL UNIQUE,
    status invoice_status NOT NULL DEFAULT 'draft',
    currency char(3) NOT NULL DEFAULT 'EUR',
    issued_on date,
    due_on date,
    notes text,
    created_at timestamptz NOT NULL DEFAULT now(),
    paid_at timestamptz
);

COMMENT ON TABLE invoices IS 'An invoice. Amounts live on its line items.';

COMMENT ON COLUMN invoices.number IS 'unique – printed as is. Never reused.';

COMMENT ON COLUMN invoices.notes IS 'Printed under the totals.
May quote the customer''s "reference" verbatim, \ included.';

CREATE TABLE line_items (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    invoice_id uuid NOT NULL REFERENCES invoices (id) ON DELETE CASCADE,
    position integer NOT NULL,
    description text NOT NULL,
    quantity numeric(12, 3) NOT NULL,
    unit_price numeric(12, 2) NOT NULL,
    vat_rate numeric(5, 4) NOT NULL,
    UNIQUE (invoice_id, position)
);

COMMENT ON COLUMN line_items.vat_rate IS 'VAT as a fraction, e.g. 0.2100.
Stored per line: rates change, issued invoices must not.';

CREATE TABLE billing.payments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id uuid NOT NULL REFERENCES invoices (id),
    method billing.payment_method NOT NULL,
    amount numeric(12, 2) NOT NULL,
    "from" text,
    received_at timestamptz NOT NULL
);

CREATE TABLE media (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id uuid NOT NULL REFERENCES invoices (id) ON DELETE CASCADE,
    filename text NOT NULL,
    content bytea NOT NULL,
    uploaded_at timestamptz NOT NULL DEFAULT now()
);

-- What each invoice has been paid. A view's columns are a model's fields, so
-- its computed column is cast as a query's is: sqlc only guesses the type of
-- a function call, and reads `sum(amount)` here as a `bigint`.
CREATE VIEW invoice_balances AS
SELECT
    invoices.id AS invoice_id,
    invoices.customer_id,
    invoices.number,
    coalesce(sum(payments.amount), 0)::numeric(12, 2) AS paid
FROM invoices LEFT JOIN billing.payments ON payments.invoice_id = invoices.id
GROUP BY invoices.id;
