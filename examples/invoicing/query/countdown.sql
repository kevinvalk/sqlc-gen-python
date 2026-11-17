-- name: Countdown :many
-- Divides by zero at its 5000th row. Its function never returns, while its
-- stream reads the rows before, as it reads each as it is iterated.
SELECT (10000 / (5000 - n))::int AS share FROM generate_series(1, 10000) AS n;
