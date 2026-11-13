-- Invoicing on MySQL: no RETURNING, so inserts report their id
-- (:execlastid); amounts are exact DECIMALs.
CREATE TABLE customers (
  id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
  name VARCHAR(100) NOT NULL,
  email VARCHAR(255),
  vip BOOL NOT NULL DEFAULT FALSE,
  preferred_method ENUM('card', 'bank-transfer', 'direct_debit'),
  metadata JSON,
  created_at DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)
) COMMENT 'Someone we send invoices to.';

CREATE TABLE invoices (
  id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
  customer_id BIGINT UNSIGNED NOT NULL,
  number VARCHAR(20) NOT NULL UNIQUE,
  status ENUM('draft', 'issued', 'paid', 'void') NOT NULL DEFAULT 'draft',
  total DECIMAL(12, 2) NOT NULL DEFAULT 0,
  issued_on DATE,
  -- Printed under the totals.
  notes TEXT,
  attachment BLOB,
  pay_within TIME,
  created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE line_items (
  invoice_id BIGINT UNSIGNED NOT NULL,
  position INT NOT NULL,
  description TEXT NOT NULL,
  quantity DECIMAL(12, 3) NOT NULL,
  unit_price DECIMAL(12, 2) NOT NULL,
  PRIMARY KEY (invoice_id, position)
);
