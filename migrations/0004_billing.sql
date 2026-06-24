-- Weekly billing invoices: one per job per week
CREATE TABLE IF NOT EXISTS invoices (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id          UUID NOT NULL REFERENCES jobs(id),
    company_id      UUID NOT NULL REFERENCES companies(id),
    professional_id UUID NOT NULL REFERENCES professionals(id),
    week_start      DATE NOT NULL,
    week_end        DATE NOT NULL,
    tasks_delivered INTEGER NOT NULL DEFAULT 0,
    hours_billed    NUMERIC(10, 2) NOT NULL DEFAULT 0,
    gross_amount    INTEGER NOT NULL,   -- cents
    platform_fee    INTEGER NOT NULL,   -- cents (15%)
    net_payout      INTEGER NOT NULL,   -- cents (85%)
    status          TEXT NOT NULL DEFAULT 'pending', -- pending | paid | failed
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    paid_at         TIMESTAMPTZ
);

-- Payouts to professionals
CREATE TABLE IF NOT EXISTS payouts (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    invoice_id      UUID NOT NULL REFERENCES invoices(id),
    professional_id UUID NOT NULL REFERENCES professionals(id),
    amount          INTEGER NOT NULL,  -- cents
    status          TEXT NOT NULL DEFAULT 'pending', -- pending | sent | failed
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    sent_at         TIMESTAMPTZ
);
